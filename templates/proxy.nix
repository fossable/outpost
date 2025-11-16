{ config, pkgs, lib, ... }:

let debug = false;
in {
  imports = [ <nixpkgs/nixos/modules/virtualisation/amazon-image.nix> ];
  ec2.hvm = true;

  # Don't replace configuration.nix with user data on boot
  systemd.services.amazon-init.enable = false;

  # Enable IP forwarding
  boot.kernel.sysctl = { "net.ipv4.ip_forward" = 1; };

  # Install required packages
  environment.systemPackages = with pkgs; [
    wireguard-tools
    iptables
    curl
    jq
    awscli2
    ec2-instance-connect
  ];

  # Configure WireGuard
  networking.wireguard.interfaces.wg0 = {
    ips = [ "172.17.0.1/24" ];
    listenPort = 51820;
    privateKey = "{PROXY_WG_PRIVATE_KEY}";

    # Set up NAT rules for port forwarding
    postSetup = ''
      ${pkgs.iptables}/bin/iptables -t nat -A PREROUTING -p {PROTOCOL} --dport {INGRESS_PORT} -j DNAT --to-destination 172.17.0.2:{ORIGIN_PORT}
      ${pkgs.iptables}/bin/iptables -t nat -A POSTROUTING -s 172.17.0.0/24 -j MASQUERADE
    '';

    postShutdown = ''
      ${pkgs.iptables}/bin/iptables -t nat -D PREROUTING -p {PROTOCOL} --dport {INGRESS_PORT} -j DNAT --to-destination 172.17.0.2:{ORIGIN_PORT} || true
      ${pkgs.iptables}/bin/iptables -t nat -D POSTROUTING -s 172.17.0.0/24 -j MASQUERADE || true
    '';

    peers = [{
      publicKey = "{ORIGIN_WG_PUBLIC_KEY}";
      presharedKey = "{PRESHARED_KEY}";
      allowedIPs = [ "172.17.0.2/32" ];
      persistentKeepalive = 25;
    }];
  };

  services.openssh = lib.mkForce false;

  users.users.root.password = lib.mkIf debug "outpost-debug";
  users.mutableUsers = false;

  # Create self-destruct monitoring service
  systemd.services.outpost-monitor = {
    description = "Outpost Origin Monitor";
    after = [ "network.target" "wireguard-wg0.service" ];
    requires = [ "wireguard-wg0.service" ];
    wantedBy = [ "multi-user.target" ];

    serviceConfig = {
      Type = "simple";
      Restart = "always";
      RestartSec = 10;
    };

    script = ''
      ORIGIN_IP="{ORIGIN_IP}"
      STACK_NAME="{STACK_NAME}"
      REGION="{REGION}"
      FAIL_COUNT=0
      MAX_FAILS=60  # 5 minutes with 5-second checks

      while true; do
        # Try to ping the origin through the WireGuard tunnel
        if ${pkgs.iputils}/bin/ping -c 1 -W 2 172.17.0.2 > /dev/null 2>&1; then
          FAIL_COUNT=0
        else
          FAIL_COUNT=$((FAIL_COUNT + 1))
          echo "$(date): Origin unreachable. Fail count: $FAIL_COUNT/$MAX_FAILS"

          if [ $FAIL_COUNT -ge $MAX_FAILS ]; then
            echo "$(date): Origin unreachable for 5 minutes. Self-destructing..."
            ${pkgs.awscli2}/bin/aws cloudformation delete-stack --stack-name "$STACK_NAME" --region "$REGION"
            exit 0
          fi
        fi

        sleep 5
      done
    '';
  };

  # Signal CloudFormation on first boot
  systemd.services.cloudformation-signal = {
    description = "Signal CloudFormation that initialization is complete";
    after = [ "network.target" "wireguard-wg0.service" ];
    requires = [ "wireguard-wg0.service" ];
    wantedBy = [ "multi-user.target" ];

    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };

    script = ''
      # Get the instance ID from EC2 metadata
      INSTANCE_ID=$(${pkgs.curl}/bin/curl -s http://169.254.169.254/latest/meta-data/instance-id)
      REGION="{REGION}"
      STACK_NAME="{STACK_NAME}"

      # Query CloudFormation to get the WaitHandle URL
      WAIT_HANDLE_URL=$(${pkgs.awscli2}/bin/aws cloudformation describe-stack-resource \
        --stack-name "$STACK_NAME" \
        --logical-resource-id WaitHandle \
        --region "$REGION" \
        --query 'StackResourceDetail.PhysicalResourceId' \
        --output text)

      # Signal success to CloudFormation
      ${pkgs.curl}/bin/curl -X PUT -H 'Content-Type: application/json' \
        --data '{"Status":"SUCCESS","Reason":"Instance initialized","UniqueId":"ProxyInstance","Data":"Ready"}' \
        "$WAIT_HANDLE_URL"
    '';
  };

  system.stateVersion = "25.05";
}
