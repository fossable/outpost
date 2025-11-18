# This is just for quickly testing the proxy EC2 instance.

packer {
  required_plugins {
    amazon = {
      version = ">= 0.0.2"
      source  = "github.com/hashicorp/amazon"
    }
  }
}

data "amazon-ami" "nixos" {
  filters = {
    name = "nixos/25.05.*-aarch64-linux"
    root-device-type    = "ebs"
    virtualization-type = "hvm"
  }
  owners      = ["427812963091"]
  most_recent = true
  region      = "us-east-2"
}

source "amazon-ebs" "outpost" {
  ami_name      = "outpost-${formatdate("YYYY-MM-DD-hhmmss", timestamp())}"
  instance_type = "t4g.large"
  region        = "us-east-2"
  source_ami    = "${data.amazon-ami.nixos.id}"
  ssh_username  = "root"
  launch_block_device_mappings {
    device_name = "/dev/xvda"
    volume_type = "gp3"
    volume_size = 10
    delete_on_termination = true
  }
}

build {
  sources = ["source.amazon-ebs.outpost"]

  provisioner "file" {
    destination = "/etc/nixos/configuration.nix"
    source = "../templates/proxy.nix"
  }

  provisioner "shell" {
    inline = [
      "sed -i 's/debug = false/debug = true/;s/{PROTOCOL}/tcp/;s/{STACK_NAME}/test/;s/{REGION}/us-east-2/;s/{ORIGIN_IP}/127.0.0.1/;s/{PRESHARED_KEY}/pZRdC6QUhuwip9nepWHXUa7dErQ+xY37cifcCFx0bb0=/;s/{ORIGIN_WG_PUBLIC_KEY}/pZRdC6QUhuwip9nepWHXUa7dErQ+xY37cifcCFx0bb0=/;s/{ORIGIN_PORT}/80/;s/{INGRESS_PORT}/80/;s/{PROXY_WG_PRIVATE_KEY}/pZRdC6QUhuwip9nepWHXUa7dErQ+xY37cifcCFx0bb0=/' /etc/nixos/configuration.nix",
      "cat /etc/nixos/configuration.nix",
      "nixos-rebuild switch",
      "sleep 1000000",
    ]
  }
}
