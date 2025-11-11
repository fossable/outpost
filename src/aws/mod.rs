pub mod cloudformation;

use anyhow::{bail, Context, Result};
use aws_config::{meta::region::RegionProviderChain, Region};
use aws_sdk_cloudformation::Client as CfnClient;
use aws_sdk_route53::Client as Route53Client;
use cloudformation::CloudFormationTemplate;
use tracing::{debug, info};

pub struct AwsProxy {
    pub stack_name: String,
    pub region: String,
    cfn_client: CfnClient,
}

/// Validates that the hosted zone exists and the ingress host is a subdomain of
/// the zone
async fn validate_route53_configuration(
    route53_client: &Route53Client,
    hosted_zone_id: &str,
    ingress_host: &str,
) -> Result<()> {
    info!("Validating Route53 configuration...");

    // Normalize the hosted zone ID (remove /hostedzone/ prefix if present)
    let zone_id = hosted_zone_id
        .trim_start_matches("/hostedzone/")
        .to_string();

    // Get the hosted zone to verify it exists and get its domain
    let hosted_zone = route53_client
        .get_hosted_zone()
        .id(&zone_id)
        .send()
        .await
        .context(format!(
            "Failed to get hosted zone '{}'. Please verify the hosted zone ID exists and you have permission to access it.",
            zone_id
        ))?;

    let zone_name = hosted_zone
        .hosted_zone()
        .map(|hz| hz.name())
        .context("Hosted zone does not have a name")?;

    // Normalize zone name (remove trailing dot if present)
    let zone_domain = zone_name.trim_end_matches('.');

    // Normalize ingress host
    let ingress_domain = ingress_host.trim_end_matches('.');

    info!("Hosted zone '{}' has domain: {}", zone_id, zone_domain);
    info!("Ingress host: {}", ingress_domain);

    // Check if ingress host is a subdomain of (or equals) the zone domain
    let is_subdomain =
        ingress_domain == zone_domain || ingress_domain.ends_with(&format!(".{}", zone_domain));

    if !is_subdomain {
        bail!(
            "Ingress host '{}' is not a subdomain of the hosted zone domain '{}'. \
            The ingress endpoint must be within the hosted zone's domain.",
            ingress_domain,
            zone_domain
        );
    }

    info!(
        "✓ Validated: '{}' is a valid subdomain of '{}'",
        ingress_domain, zone_domain
    );

    Ok(())
}

impl AwsProxy {
    pub async fn deploy(
        ingress_host: String,
        ingress_port: u16,
        ingress_protocol: String,
        origin_host: String,
        origin_port: u16,
        origin_ip: String,
        regions: Vec<String>,
        instance_type: String,
        proxy_wg_private_key: String,
        proxy_wg_public_key: String,
        origin_wg_public_key: String,
        preshared_key: String,
        hosted_zone_id: String,
    ) -> Result<Self> {
        // Use the first region in the list, or fall back to defaults
        let region = regions
            .first()
            .cloned()
            .unwrap_or_else(|| "us-east-2".to_string());

        info!("Deploying AWS proxy in region: {}", region);

        let region_provider = RegionProviderChain::first_try(Region::new(region.clone()))
            .or_default_provider()
            .or_else(Region::new("us-east-2"));

        let config = aws_config::from_env().region(region_provider).load().await;
        let cfn_client = CfnClient::new(&config);
        let route53_client = Route53Client::new(&config);

        // Validate Route53 configuration before proceeding
        validate_route53_configuration(&route53_client, &hosted_zone_id, &ingress_host).await?;

        // Generate unique stack name
        let stack_name = format!("outpost-{}", ingress_host.replace(".", "-"));

        // Generate CloudFormation template
        let template = CloudFormationTemplate {
            stack_name: stack_name.clone(),
            region: region.clone(),
            ingress_host: ingress_host.clone(),
            ingress_port,
            ingress_protocol,
            origin_host,
            origin_port,
            origin_ip,
            instance_type,
            proxy_wg_private_key,
            proxy_wg_public_key,
            origin_wg_public_key,
            preshared_key,
        };

        let template_body = template.generate()?;
        debug!("Generated CloudFormation template:\n{}", template_body);

        // Deploy the CloudFormation stack
        info!("Creating CloudFormation stack: {}", stack_name);
        cfn_client
            .create_stack()
            .stack_name(&stack_name)
            .template_body(template_body)
            .parameters(
                aws_sdk_cloudformation::types::Parameter::builder()
                    .parameter_key("HostedZoneId")
                    .parameter_value(hosted_zone_id)
                    .build(),
            )
            .capabilities(aws_sdk_cloudformation::types::Capability::CapabilityIam)
            .send()
            .await
            .context("Failed to create CloudFormation stack")?;

        info!("CloudFormation stack creation initiated: {}", stack_name);

        Ok(Self {
            stack_name,
            region,
            cfn_client,
        })
    }

    /// Wait for the CloudFormation stack to complete deployment
    pub async fn wait_for_completion(&self) -> Result<String> {
        info!(
            "Waiting for CloudFormation stack to complete: {}",
            self.stack_name
        );

        loop {
            let response = self
                .cfn_client
                .describe_stacks()
                .stack_name(&self.stack_name)
                .send()
                .await
                .context("Failed to describe CloudFormation stack")?;

            let stack = response
                .stacks()
                .first()
                .context("Stack not found in describe_stacks response")?;

            let status = stack
                .stack_status()
                .context("Stack does not have a status")?;
            info!("Stack status: {:?}", status);

            use aws_sdk_cloudformation::types::StackStatus;
            match status {
                StackStatus::CreateComplete => {
                    info!("Stack creation completed successfully");

                    // Extract the proxy public IP from outputs
                    let proxy_ip = stack
                        .outputs()
                        .iter()
                        .find(|output| output.output_key() == Some("ProxyPublicIP"))
                        .and_then(|output| output.output_value())
                        .context("ProxyPublicIP output not found in stack")?;

                    info!("Proxy public IP: {}", proxy_ip);
                    return Ok(proxy_ip.to_string());
                }
                StackStatus::CreateInProgress => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(20)).await;
                }
                StackStatus::CreateFailed
                | StackStatus::RollbackComplete
                | StackStatus::RollbackFailed
                | StackStatus::RollbackInProgress => {
                    let reason = stack.stack_status_reason().unwrap_or("Unknown reason");
                    bail!("Stack creation failed: {} - {}", status.as_str(), reason);
                }
                _ => {
                    bail!("Unexpected stack status: {}", status.as_str());
                }
            }
        }
    }

    /// Gracefully delete the CloudFormation stack
    pub async fn cleanup(&self) -> Result<()> {
        info!("Cleaning up CloudFormation stack: {}", self.stack_name);

        self.cfn_client
            .delete_stack()
            .stack_name(&self.stack_name)
            .send()
            .await
            .context("Failed to delete CloudFormation stack")?;

        info!(
            "CloudFormation stack deletion initiated: {}",
            self.stack_name
        );
        Ok(())
    }
}

impl Drop for AwsProxy {
    fn drop(&mut self) {
        info!(
            "AwsProxy dropped - if not cleaned up, stack {} will self-destruct when origin is unreachable",
            self.stack_name
        );
    }
}
