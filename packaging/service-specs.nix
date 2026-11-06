# Canonical ownership for NixOS, Home Manager, standalone units and activation.
{
  bar-daemon = {
    role = "status";
    bus = "org.laufan.BarStatusDaemon";
    description = "Desktop status and actions";
    aliases = [ ];
  };
  bar-notification-daemon = {
    role = "notifications";
    bus = "org.laufan.BarNotificationDaemon";
    description = "Desktop notifications";
    aliases = [ "org.freedesktop.Notifications" ];
  };
  bar-power-daemon = {
    role = "power";
    bus = "org.laufan.BarPowerDaemon";
    description = "Desktop power and sleep policy";
    aliases = [ ];
  };
  bar-compositor-daemon = {
    role = "compositor";
    bus = "org.laufan.BarCompositorDaemon";
    description = "Compositor integration and display policy";
    aliases = [ ];
  };
  bar-activity-daemon = {
    role = "activity";
    bus = "org.laufan.BarActivityDaemon";
    description = "Agenda, locations, clocks and weather";
    aliases = [ "org.laufan.BarDaemon" ];
  };
}
