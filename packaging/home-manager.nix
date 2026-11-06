self:
{
  config,
  lib,
  pkgs,
  ...
}:
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
    systemd.user.services.bar-notification-daemon = {
      Unit = {
        Description = "Desktop notifications";
        PartOf = [ cfg.target ];
        After = [
          cfg.target
          "dbus.service"
        ];
        Conflicts = [ "swaync.service" ];
        Before = [ "swaync.service" ];
      };
      Service = {
        Type = "dbus";
        BusName = "org.laufan.BarNotificationDaemon";
        ExecStart = "${cfg.package}/bin/bar-notification-daemon daemon";
        Restart = "on-failure";
        RestartSec = "2s";
        Environment = [ "BAR_DAEMON_NOTIFICATION_BACKEND=native" ];
      };
      Install.WantedBy = [ cfg.target ];
    };
    systemd.user.services.bar-power-daemon = {
      Unit = {
        Description = "Desktop power and sleep policy";
        PartOf = [ cfg.target ];
        After = [
          cfg.target
          "dbus.service"
        ];
      };
      Service = {
        Type = "dbus";
        BusName = "org.laufan.BarPowerDaemon";
        ExecStart = "${cfg.package}/bin/bar-power-daemon daemon";
        Restart = "on-failure";
        RestartSec = "2s";
        Environment = lib.optional cfg.suspend.enable "BAR_DAEMON_IDLE_CONFIG=${idleConfig}";
      };
      Install.WantedBy = [ cfg.target ];
    };
    systemd.user.services.bar-compositor-daemon = {
      Unit = {
        Description = "Compositor integration and display policy";
        PartOf = [ cfg.target ];
        After = [
          cfg.target
          "dbus.service"
        ]
        ++ lib.optional cfg.displays.enable "hypr-monitor-auto.service";
        Conflicts = lib.optional cfg.displays.enable "hypr-monitor-auto.service";
      };
      Service = {
        Type = "dbus";
        BusName = "org.laufan.BarCompositorDaemon";
        ExecStart = "${cfg.package}/bin/bar-compositor-daemon daemon";
        Restart = "on-failure";
        RestartSec = "2s";
        Environment = lib.optional cfg.displays.enable "BAR_DAEMON_DISPLAY_CONTROL=1";
      };
      Install.WantedBy = [ cfg.target ];
    };
    systemd.user.services.bar-activity-daemon = {
      Unit = {
        Description = "Agenda, locations, clocks and weather";
        PartOf = [ cfg.target ];
        After = [
          cfg.target
          "dbus.service"
        ];
      };
      Service = {
        Type = "dbus";
        BusName = "org.laufan.BarActivityDaemon";
        ExecStart = "${cfg.package}/bin/bar-activity-daemon daemon";
        Restart = "on-failure";
        RestartSec = "2s";
      };
      Install.WantedBy = [ cfg.target ];
    };
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
