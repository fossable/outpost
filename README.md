<p align="center">
	<img src="https://raw.githubusercontent.com/fossable/turbine/master/.github/images/outpost-256.png" />
</p>

![License](https://img.shields.io/github/license/fossable/outpost)
![Build](https://github.com/fossable/outpost/actions/workflows/test.yml/badge.svg)
![GitHub repo size](https://img.shields.io/github/repo-size/fossable/outpost)
![Stars](https://img.shields.io/github/stars/fossable/outpost?style=social)

<hr>

**outpost** allows you to expose self-hosted web services to the Internet via
popular cloud providers.

### Cloudflare

HTTP sites can be hosted with Cloudflare easily:

```yml
name: example_com

services:
  outpost:
    image: fossable/outpost:latest
    depends_on:
      - www
    environment:
      OUTPOST_CLOUDFLARE_INGRESS: tls://www.example.com:443
      OUTPOST_CLOUDFLARE_ORIGIN: tcp://www:80
      OUTPOST_CLOUDFLARE_ORIGIN_CERT: |
        -----BEGIN PRIVATE KEY-----

  www:
    image: httpd:latest
```

### AWS

`outpost` can also use an EC2 proxy to expose any TCP/UDP port. The proxy
instance communicates with the origin service via an ephemeral WireGuard tunnel.

The AWS deployment uses CloudFormation to create:
- A new VPC with public subnet and internet gateway
- An EC2 instance with WireGuard configured
- Security groups that only allow WireGuard from the origin IP and configured ingress ports
- Route53 DNS record pointing to the proxy
- IAM role allowing the instance to self-destruct if the origin is unreachable for 5+ minutes

**Note:** WireGuard key generation uses boringtun, so no WireGuard installation is required on the host.

```yml
name: example_com

services:
  outpost:
    image: fossable/outpost:latest
    depends_on:
      - www
    environment:
      OUTPOST_AWS_INGRESS: tcp://www.example.com:80
      OUTPOST_AWS_ORIGIN: tcp://www:8080
      OUTPOST_AWS_REGIONS: us-east-2
      OUTPOST_AWS_HOSTED_ZONE_ID: Z1234567890ABC
      AWS_ACCESS_KEY_ID: <...>
      AWS_SECRET_ACCESS_KEY: <...>

  www:
    image: httpd:latest
```
