pub mod cloudformation;

use anyhow::{bail, Context, Result};
use aws_config::{meta::region::RegionProviderChain, Region};
use aws_sdk_cloudformation::Client as CfnClient;
use aws_sdk_ec2::Client as Ec2Client;
use aws_sdk_route53::Client as Route53Client;
use cloudformation::CloudFormationTemplate;
use indicatif::{ProgressBar, ProgressStyle};
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

/// Query the latest NixOS AMI for the given architecture
async fn get_latest_nixos_ami(ec2_client: &Ec2Client, architecture: &str) -> Result<String> {
    debug!(
        "Looking up latest NixOS AMI for architecture: {}",
        architecture
    );

    // NixOS official AWS account ID
    const NIXOS_OWNER_ID: &str = "427812963091";

    let images = ec2_client
        .describe_images()
        .owners(NIXOS_OWNER_ID)
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("name")
                .values("nixos/25.05*")
                .build(),
        )
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("architecture")
                .values(architecture)
                .build(),
        )
        .send()
        .await
        .context("Failed to query NixOS AMIs from EC2")?;

    let mut images_list = images.images().to_vec();

    if images_list.is_empty() {
        bail!(
            "No NixOS AMIs found for architecture {}. Please check the region supports NixOS AMIs.",
            architecture
        );
    }

    // Sort by creation date (newest first)
    images_list.sort_by(|a, b| {
        let date_a = a.creation_date().unwrap_or("");
        let date_b = b.creation_date().unwrap_or("");
        date_b.cmp(date_a)
    });

    let latest_ami = images_list.first().context("No AMIs found after sorting")?;

    let ami_id = latest_ami
        .image_id()
        .context("AMI does not have an image ID")?;

    info!(
        "Found latest NixOS AMI: {} (created: {})",
        ami_id,
        latest_ami.creation_date().unwrap_or("unknown")
    );

    Ok(ami_id.to_string())
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
        debug: bool,
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
        let ec2_client = Ec2Client::new(&config);
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
            debug,
        };

        let template_body = template.generate()?;
        debug!("Generated CloudFormation template:\n{}", template_body);

        // Query for the latest NixOS AMI
        let ami_id = get_latest_nixos_ami(&ec2_client, template.get_architecture()).await?;

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
            .parameters(
                aws_sdk_cloudformation::types::Parameter::builder()
                    .parameter_key("NixOSAMI")
                    .parameter_value(ami_id)
                    .build(),
            )
            .capabilities(aws_sdk_cloudformation::types::Capability::CapabilityIam)
            .on_failure(aws_sdk_cloudformation::types::OnFailure::Delete)
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

        // Check if we're connected to a TTY
        let is_tty = atty::is(atty::Stream::Stdout);

        let progress = if is_tty {
            let pb = ProgressBar::new(1);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("━━╸"),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            Some(pb)
        } else {
            None
        };

        let mut completed_resources = std::collections::HashSet::new();
        let mut total_resources = 0u64;

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

            // Get stack events to track progress
            if let Some(ref pb) = progress {
                if let Ok(events) = self
                    .cfn_client
                    .describe_stack_events()
                    .stack_name(&self.stack_name)
                    .send()
                    .await
                {
                    // Count total unique resources and completed ones
                    for event in events.stack_events() {
                        if let Some(resource_id) = event.logical_resource_id() {
                            // Skip the stack itself
                            if resource_id == self.stack_name {
                                continue;
                            }

                            // Track total unique resources
                            total_resources = total_resources.max(
                                events
                                    .stack_events()
                                    .iter()
                                    .filter(|e| {
                                        e.logical_resource_id()
                                            .map_or(false, |id| id != self.stack_name)
                                    })
                                    .filter_map(|e| e.logical_resource_id())
                                    .collect::<std::collections::HashSet<_>>()
                                    .len() as u64,
                            );

                            // Track completed resources
                            if let Some(status) = event.resource_status() {
                                let status_str = status.as_str();
                                if status_str.ends_with("_COMPLETE")
                                    && !status_str.starts_with("DELETE")
                                {
                                    completed_resources.insert(resource_id.to_string());
                                }
                            }
                        }
                    }

                    // Update progress bar
                    if total_resources > 0 {
                        pb.set_length(total_resources);
                        pb.set_position(completed_resources.len() as u64);
                    }

                    // Show the latest event
                    if let Some(latest_event) = events.stack_events().first() {
                        let resource = latest_event.logical_resource_id().unwrap_or("Stack");
                        let event_status = latest_event
                            .resource_status()
                            .map(|s| s.as_str())
                            .unwrap_or("UNKNOWN");
                        let reason = latest_event.resource_status_reason().unwrap_or("");

                        let msg = if reason.is_empty() {
                            format!("{}: {}", resource, event_status)
                        } else {
                            format!("{}: {} - {}", resource, event_status, reason)
                        };
                        pb.set_message(msg);
                    }
                }
            } else {
                info!("Stack status: {:?}", status);
            }

            use aws_sdk_cloudformation::types::StackStatus;
            match status {
                StackStatus::CreateComplete => {
                    if let Some(pb) = progress {
                        pb.finish_with_message("Stack creation completed successfully");
                    } else {
                        info!("Stack creation completed successfully");
                    }

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
                StackStatus::CreateInProgress | StackStatus::DeleteInProgress => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
                StackStatus::CreateFailed
                | StackStatus::RollbackComplete
                | StackStatus::RollbackFailed
                | StackStatus::RollbackInProgress
                | StackStatus::DeleteFailed
                | StackStatus::DeleteComplete => {
                    let reason = stack.stack_status_reason().unwrap_or("Unknown reason");
                    if let Some(pb) = progress {
                        pb.finish_with_message(format!(
                            "Stack creation failed: {} - {}",
                            status.as_str(),
                            reason
                        ));
                    }
                    bail!("Stack creation failed: {} - {}", status.as_str(), reason);
                }
                _ => {
                    if let Some(pb) = progress {
                        pb.finish_with_message(format!(
                            "Unexpected stack status: {}",
                            status.as_str()
                        ));
                    }
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

        // Check if we're connected to a TTY
        let is_tty = atty::is(atty::Stream::Stdout);

        let progress = if is_tty {
            let pb = ProgressBar::new(1);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{bar:40.cyan/blue} {pos}/{len} {msg}")
                    .unwrap()
                    .progress_chars("━━╸"),
            );
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            Some(pb)
        } else {
            None
        };

        let mut deleted_resources = std::collections::HashSet::new();
        let mut total_resources = 0u64;

        // Wait for the stack deletion to complete
        if progress.is_none() {
            info!("Waiting for stack deletion to complete...");
        }

        loop {
            let response = self
                .cfn_client
                .describe_stacks()
                .stack_name(&self.stack_name)
                .send()
                .await;

            match response {
                Ok(resp) => {
                    let stack = resp
                        .stacks()
                        .first()
                        .context("Stack not found in describe_stacks response")?;

                    let status = stack
                        .stack_status()
                        .context("Stack does not have a status")?;

                    // Get stack events to track deletion progress
                    if let Some(ref pb) = progress {
                        if let Ok(events) = self
                            .cfn_client
                            .describe_stack_events()
                            .stack_name(&self.stack_name)
                            .send()
                            .await
                        {
                            // Count total unique resources and deleted ones
                            for event in events.stack_events() {
                                if let Some(resource_id) = event.logical_resource_id() {
                                    // Skip the stack itself
                                    if resource_id == self.stack_name {
                                        continue;
                                    }

                                    // Track total unique resources
                                    total_resources = total_resources.max(
                                        events
                                            .stack_events()
                                            .iter()
                                            .filter(|e| {
                                                e.logical_resource_id()
                                                    .map_or(false, |id| id != self.stack_name)
                                            })
                                            .filter_map(|e| e.logical_resource_id())
                                            .collect::<std::collections::HashSet<_>>()
                                            .len() as u64,
                                    );

                                    // Track deleted resources
                                    if let Some(status) = event.resource_status() {
                                        let status_str = status.as_str();
                                        if status_str == "DELETE_COMPLETE" {
                                            deleted_resources.insert(resource_id.to_string());
                                        }
                                    }
                                }
                            }

                            // Update progress bar
                            if total_resources > 0 {
                                pb.set_length(total_resources);
                                pb.set_position(deleted_resources.len() as u64);
                            }

                            // Show the latest event
                            if let Some(latest_event) = events.stack_events().first() {
                                let resource =
                                    latest_event.logical_resource_id().unwrap_or("Stack");
                                let event_status = latest_event
                                    .resource_status()
                                    .map(|s| s.as_str())
                                    .unwrap_or("UNKNOWN");
                                let reason = latest_event.resource_status_reason().unwrap_or("");

                                let msg = if reason.is_empty() {
                                    format!("{}: {}", resource, event_status)
                                } else {
                                    format!("{}: {} - {}", resource, event_status, reason)
                                };
                                pb.set_message(msg);
                            }
                        }
                    } else {
                        info!("Stack deletion status: {:?}", status);
                    }

                    use aws_sdk_cloudformation::types::StackStatus;
                    match status {
                        StackStatus::DeleteComplete => {
                            if let Some(pb) = progress {
                                pb.finish_with_message("Stack deletion completed successfully");
                            } else {
                                info!("Stack deletion completed successfully");
                            }
                            return Ok(());
                        }
                        StackStatus::DeleteInProgress => {
                            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        }
                        StackStatus::DeleteFailed => {
                            let reason = stack.stack_status_reason().unwrap_or("Unknown reason");
                            if let Some(pb) = progress {
                                pb.finish_with_message(format!(
                                    "Stack deletion failed: {}",
                                    reason
                                ));
                            }
                            bail!("Stack deletion failed: {}", reason);
                        }
                        _ => {
                            let reason = stack.stack_status_reason().unwrap_or("Unknown reason");
                            if let Some(pb) = progress {
                                pb.finish_with_message(format!(
                                    "Unexpected stack status: {} - {}",
                                    status.as_str(),
                                    reason
                                ));
                            }
                            bail!(
                                "Unexpected stack status during deletion: {} - {}",
                                status.as_str(),
                                reason
                            );
                        }
                    }
                }
                Err(e) => {
                    // If the stack doesn't exist anymore, that's actually success
                    // Check for various error conditions that indicate the stack is gone
                    let error_str = format!("{:?}", e);
                    if error_str.contains("ValidationError")
                        || error_str.contains("does not exist")
                        || error_str.contains("Stack with id")
                    {
                        if let Some(pb) = progress {
                            pb.finish_with_message("Stack has been deleted");
                        } else {
                            info!("Stack has been deleted (no longer queryable)");
                        }
                        return Ok(());
                    }
                    // Log the actual error for debugging
                    tracing::warn!("Unexpected error checking stack deletion status: {:?}", e);
                    return Err(e).context("Failed to check stack deletion status");
                }
            }
        }
    }
}
