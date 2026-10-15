self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.bar-daemon;
  idleConfig = "${config.xdg.configHome}/hypr/hypridle.conf";
  managedHypridle = self.lib.mkManagedHypridle config.services.hypridle.package;
in
{
  options.services.bar-daemon = import ./session-options.nix {
    inherit lib;
    package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  };
  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    assertions = lib.optional cfg.suspend.enable {
      assertion = config.services.hypridle.enable && config.services.hypridle.package != null;
      message = "services.bar-daemon.suspend requires Home Manager's hypridle service.";
    };
    systemd.user.services.bar-daemon = import ./session-service.nix { inherit lib cfg idleConfig; };
    systemd.user.services.hypridle = lib.mkIf cfg.suspend.enable {
      Service = {
        Type = lib.mkForce "notify";
        NotifyAccess = "main";
        TimeoutStartSec = "8s";
        ExecStart = lib.mkForce "${cfg.package}/bin/bar-daemon idle --config ${lib.escapeShellArg idleConfig} --hypridle ${managedHypridle}/bin/hypridle";
      };
    };
  };
}
