{ lib, cfg, idleConfig }:
{
  Unit = {
    Description = "Desktop status and policy service";
    PartOf = [ cfg.target ];
    After = [ cfg.target "dbus.service" "pipewire.service" "wireplumber.service" ]
      ++ lib.optional cfg.displays.enable "hypr-monitor-auto.service";
    Before = [ "swaync.service" ];
    Conflicts = [ "swaync.service" ] ++ lib.optional cfg.displays.enable "hypr-monitor-auto.service";
  };
  Service = {
    Type = "dbus";
    BusName = "org.laufan.BarDaemon";
    ExecStart = "${cfg.package}/bin/bar-daemon daemon";
    Environment = [ "BAR_DAEMON_NOTIFICATION_BACKEND=native" ]
      ++ lib.optional cfg.suspend.enable "BAR_DAEMON_IDLE_CONFIG=${idleConfig}"
      ++ lib.optional cfg.displays.enable "BAR_DAEMON_DISPLAY_CONTROL=1"
      ++ lib.optional cfg.media.youtubeMetadata.enable "BAR_DAEMON_YOUTUBE_METADATA=1";
    Restart = "on-failure";
    RestartSec = "2s";
  };
  Install.WantedBy = [ cfg.target ];
}
