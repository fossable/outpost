<p align="center">
	<img src="https://raw.githubusercontent.com/fossable/turbine/master/.github/images/outpost-256.png" />
</p>

![License](https://img.shields.io/github/license/fossable/outpost)
![Build](https://github.com/fossable/outpost/actions/workflows/test.yml/badge.svg)
![GitHub repo size](https://img.shields.io/github/repo-size/fossable/outpost)
![Stars](https://img.shields.io/github/stars/fossable/outpost?style=social)

<hr>

**outpost** allows you to expose self-hosted web services to the Internet via
popular cloud providers without getting "locked in" to any particular cloud.

## Why self-host?

Cloud hosting has undeniable advantages over pure self hosting:

- effectively infinite compute resources with extremely high reliability
- low latency hosting in any geographic location
- no blocked inbound ports by ISP
- high upload speed

To get these benefits, you give up some control of your applications. And, since
your application likely depends on your cloud provider to varying degrees, you
don't have the option to move to a different provider without some migration
effort.

Since it would be extremely costly to build a datacenter yourself, the easiest
way is a hybrid approach. **outpost** sets up the provider-specific
infrastructure which makes it simple to move to another cloud.

### Cloudflare

HTTP sites can be hosted with Cloudflare:

```yml
name: example_com

services:
  outpost:
    image: fossable/outpost:latest
    depends_on:
      - origin_www
    environment:
      OUTPOST_CLOUDFLARE_INGRESS: tls://www.example.com:443
      OUTPOST_CLOUDFLARE_ORIGIN: tcp://origin_www:80
      OUTPOST_CLOUDFLARE_ORIGIN_CERT: |
        -----BEGIN PRIVATE KEY-----

  origin_www:
    image: httpd:latest
```

This takes advantage of Cloudflare for TLS cert generation and their CDN.

### AWS

`outpost` can also use an EC2 proxy to expose any TCP/UDP port. The proxy
instance communicates with the origin service via an ephemeral WireGuard tunnel.

The AWS deployment uses CloudFormation to create:

```yml
name: example_com

services:
  outpost:
    image: fossable/outpost:latest
    cap_add:
      - NET_ADMIN
    depends_on:
      - origin_www
    environment:
      OUTPOST_AWS_INGRESS: tcp://www.example.com:80
      OUTPOST_AWS_ORIGIN: tcp://origin_www:8080
      OUTPOST_AWS_REGIONS: us-east-2 # TODO only one
      OUTPOST_AWS_HOSTED_ZONE_ID: Z1234567890ABC
      AWS_ACCESS_KEY_ID: <...>
      AWS_SECRET_ACCESS_KEY: <...>

  origin_www:
    image: httpd:latest
```
