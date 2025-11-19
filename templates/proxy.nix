{ config, pkgs, lib, ... }:

let debug = false;
in {
  imports = [ <nixpkgs/nixos/modules/virtualisation/amazon-image.nix> ];
  ec2.hvm = true;

  # Don't replace configuration.nix with user data on boot
  systemd.services.amazon-init.enable = false;

  # Enable IP forwarding
  boot.kernel.sysctl = { "net.ipv4.ip_forward" = 1; };

  # Install only essential packages (keep minimal)
  environment.systemPackages = with pkgs; [
    wireguard-tools
    iptables
    curl
    awscli2
  ];

  # Don't install default packages
  environment.defaultPackages = [ ];

  # Configure WireGuard
  networking.wireguard.interfaces.wg0 = {
    ips = [ "{PROXY_IP}/24" ];
    listenPort = 51820;
    privateKey = "{PROXY_WG_PRIVATE_KEY}";

    # Set up NAT rules for port forwarding
    postSetup = ''
      ${pkgs.iptables}/bin/iptables -t nat -A PREROUTING -p {PROTOCOL} --dport {INGRESS_PORT} -j DNAT --to-destination {ORIGIN_IP}:{ORIGIN_PORT}
      ${pkgs.iptables}/bin/iptables -t nat -A POSTROUTING -d {ORIGIN_IP}/32 -p {PROTOCOL} --dport {ORIGIN_PORT} -j MASQUERADE
      ${pkgs.iptables}/bin/iptables -t nat -A POSTROUTING -s {SUBNET}/24 -j MASQUERADE
    '';

    postShutdown = ''
      ${pkgs.iptables}/bin/iptables -t nat -D PREROUTING -p {PROTOCOL} --dport {INGRESS_PORT} -j DNAT --to-destination {ORIGIN_IP}:{ORIGIN_PORT} || true
      ${pkgs.iptables}/bin/iptables -t nat -D POSTROUTING -d {ORIGIN_IP}/32 -p {PROTOCOL} --dport {ORIGIN_PORT} -j MASQUERADE || true
      ${pkgs.iptables}/bin/iptables -t nat -D POSTROUTING -s {SUBNET}/24 -j MASQUERADE || true
    '';

    peers = [{
      publicKey = "{ORIGIN_WG_PUBLIC_KEY}";
      presharedKey = "{PRESHARED_KEY}";
      allowedIPs = [ "{ORIGIN_IP}/32" ];
      persistentKeepalive = 25;
    }];
  };

  services = {
    openssh = {
      enable = lib.mkForce debug;
      settings.PasswordAuthentication = lib.mkForce true;
      settings.PermitRootLogin = lib.mkForce "yes";
    };
    amazon-ssm-agent.enable = pkgs.lib.mkForce false;
  };

  users = {
    mutableUsers = false;
    allowNoPasswordLogin = !debug;
    users.root.password = lib.mkIf debug "outpost-debug";
  };

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
      FAIL_COUNT=0
      MAX_FAILS=60  # 5 minutes with 5-second checks

      while true; do
        # Try to ping the origin through the WireGuard tunnel
        if ${pkgs.iputils}/bin/ping -c 1 -W 2 {ORIGIN_IP} > /dev/null 2>&1; then
          FAIL_COUNT=0
        else
          FAIL_COUNT=$((FAIL_COUNT + 1))
          echo "$(date): Origin unreachable. Fail count: $FAIL_COUNT/$MAX_FAILS"

          if [ $FAIL_COUNT -ge $MAX_FAILS ]; then
            echo "$(date): Origin unreachable for 5 minutes. Self-destructing..."
            ${pkgs.awscli2}/bin/aws cloudformation delete-stack --stack-name "{STACK_NAME}" --region "{REGION}"
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
      # Query CloudFormation to get the WaitHandle URL
      WAIT_HANDLE_URL=$(${pkgs.awscli2}/bin/aws cloudformation describe-stack-resource \
        --stack-name "{STACK_NAME}" \
        --logical-resource-id WaitHandle \
        --region "{REGION}" \
        --query 'StackResourceDetail.PhysicalResourceId' \
        --output text)

      # Signal success to CloudFormation
      ${pkgs.curl}/bin/curl -X PUT -H 'Content-Type: application/json' \
        --data '{"Status":"SUCCESS","Reason":"Instance initialized","UniqueId":"ProxyInstance","Data":"Ready"}' \
        "$WAIT_HANDLE_URL"
    '';
  };

  # Disable unnecessary services and features
  security.sudo.enable = false;
  networking.firewall.enable = false;

  # Disable documentation to save space
  documentation.enable = false;
  documentation.man.enable = false;
  documentation.info.enable = false;
  documentation.doc.enable = false;
  documentation.nixos.enable = false;

  # Reduce journal size
  services.journald.extraConfig = ''
    SystemMaxUse=100M
    MaxRetentionSec=1day
  '';

  system.stateVersion = "25.05";
}
