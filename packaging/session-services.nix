{
  lib,
  cfg,
  idleConfig,
}:
lib.mapAttrs (name: spec: {
  Unit = {
    Description = spec.description;
    PartOf = [ cfg.target ];
    After = [
      cfg.target
      "dbus.service"
    ]
    ++ lib.optionals (spec.role == "status") [
      "pipewire.service"
      "wireplumber.service"
    ]
    ++ lib.optional (spec.role == "compositor" && cfg.displays.enable) "hypr-monitor-auto.service";
    Conflicts =
      lib.optional (spec.role == "notifications") "swaync.service"
      ++ lib.optional (spec.role == "compositor" && cfg.displays.enable) "hypr-monitor-auto.service";
    Before = lib.optional (spec.role == "notifications") "swaync.service";
  };
  Service = {
    Type = "dbus";
    BusName = spec.bus;
    ExecStart = "${cfg.package}/bin/${name} daemon";
    Environment =
      lib.optional (spec.role == "notifications") "BAR_DAEMON_NOTIFICATION_BACKEND=native"
      ++ lib.optional (
        spec.role == "status" && cfg.media.youtubeMetadata.enable
      ) "BAR_DAEMON_YOUTUBE_METADATA=1"
      ++ lib.optional (spec.role == "power" && cfg.suspend.enable) "BAR_DAEMON_IDLE_CONFIG=${idleConfig}"
      ++ lib.optional (spec.role == "compositor" && cfg.displays.enable) "BAR_DAEMON_DISPLAY_CONTROL=1";
    Restart = "on-failure";
    RestartSec = "2s";
  };
  Install.WantedBy = [ cfg.target ];
}) (import ./service-specs.nix)
