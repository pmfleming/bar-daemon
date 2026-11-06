self:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.bar-daemon;
  battery = import ./battery-integration.nix { inherit pkgs; package = cfg.package; };
  session = import ./session-service.nix { inherit lib cfg; idleConfig = cfg.idleConfig; };
in
{
  options.services.bar-daemon = (import ./session-options.nix {
    inherit lib;
    package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
  }) // {
    batteryHelper.enable = lib.mkEnableOption "privileged battery helper and its authorization policy";
    idleConfig = lib.mkOption {
      type = lib.types.str;
      default = "%h/.config/hypr/hypridle.conf";
      description = "Idle configuration for the daemon-managed notify-ready hypridle service.";
    };
  };
  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      environment.systemPackages = [ cfg.package ];
      services.dbus.packages = [ cfg.package ];
      systemd.user.services.bar-daemon = {
        unitConfig = session.Unit;
        serviceConfig = session.Service;
        wantedBy = session.Install.WantedBy;
      };
      systemd.user.services.bar-notification-daemon = {
        description = "Desktop notifications"; partOf = [ cfg.target ]; after = [ cfg.target "dbus.service" ]; wantedBy = [ cfg.target ]; conflicts = [ "swaync.service" ]; before = [ "swaync.service" ];
        serviceConfig = { Type = "dbus"; BusName = "org.laufan.BarNotificationDaemon"; ExecStart = "${cfg.package}/bin/bar-notification-daemon daemon"; Restart = "on-failure"; RestartSec = "2s"; Environment = [ "BAR_DAEMON_NOTIFICATION_BACKEND=native" ]; };
      };
      systemd.user.services.bar-power-daemon = {
        description = "Desktop power and sleep policy"; partOf = [ cfg.target ]; after = [ cfg.target "dbus.service" ]; wantedBy = [ cfg.target ];
        serviceConfig = { Type = "dbus"; BusName = "org.laufan.BarPowerDaemon"; ExecStart = "${cfg.package}/bin/bar-power-daemon daemon"; Restart = "on-failure"; RestartSec = "2s"; Environment = lib.optional cfg.suspend.enable "BAR_DAEMON_IDLE_CONFIG=${cfg.idleConfig}"; };
      };
      systemd.user.services.hypridle = lib.mkIf cfg.suspend.enable {
        description = "Daemon-managed idle policy";
        wantedBy = [ cfg.target ];
        partOf = [ cfg.target ];
        after = [ cfg.target ];
        serviceConfig = {
          Type = "notify";
          NotifyAccess = "main";
          TimeoutStartSec = "8s";
          ExecStart = "${cfg.package}/bin/bar-daemon idle --config ${cfg.idleConfig} --hypridle ${self.lib.mkManagedHypridle pkgs.hypridle}/bin/hypridle";
          Restart = "on-failure";
        };
      };
    })
    (lib.mkIf cfg.batteryHelper.enable {
      environment.systemPackages = [ battery ];
      services.dbus.packages = [ battery ];
      systemd.packages = [ battery ];
      security.polkit.enable = true;
    })
  ];
}
