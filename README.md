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
      OUTPOST_CONFIG: |
        {
          "www.example.com": {
            "service": "www",
            "provider": "cloudflare",
            "ports": ["80:443"]
          }
        }
      OUTPOST_CLOUDFLARE_ORIGIN_CERT: |
        -----BEGIN PRIVATE KEY-----

  www:
    image: httpd:latest
```

### AWS

`outpost` can also use an EC2 proxy to expose any TCP/UDP port. The proxy instance
communicates with the origin service via an ephemeral wireguard tunnel.

```yml
name: example_com

services:
  outpost:
    image: fossable/outpost:latest
    depends_on:
      - www
    cap_add:
      - NET_ADMIN
      - SYS_MODULE
    volumes:
      - /lib/modules:/lib/modules
    ports:
      - 51820:51820/udp
    sysctls:
      - net.ipv4.conf.all.src_valid_mark=1
    environment:
      OUTPOST_CONFIG: |
        {
          "www.example.com": {
            "service": "www",
            "provider": "aws",
            "ports": ["80:443"]
          }
        }
      AWS_ACCESS_KEY_ID: <...>
      AWS_SECRET_ACCESS_KEY: <...>

  www:
    image: httpd:latest
```

