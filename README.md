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

### Example with docker-compose

```yml
version: '3.8'

services:
  outpost:
    image: fossable/outpost:latest
    depends_on:
      - www
  www:
    build:
      context: www
```

