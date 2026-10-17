{ self, nixpkgs, system, pkgs }:
let
  evaluate = settings: (nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [ self.nixosModules.default settings ];
  }).config;
  session = evaluate {
    services.bar-daemon = {
      enable = true;
      displays.enable = true;
      suspend.enable = true;
      media.youtubeMetadata.enable = true;
    };
  };
  helper = evaluate { services.bar-daemon.batteryHelper.enable = true; };
  location = (nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [ self.nixosModules.location-timezone {
      services.location-timezone.enable = true;
      networking.networkmanager.enable = true;
    } ];
  }).config;
in
assert session.systemd.user.services.bar-daemon.serviceConfig.BusName == "org.laufan.BarDaemon";
assert builtins.elem "BAR_DAEMON_DISPLAY_CONTROL=1" session.systemd.user.services.bar-daemon.serviceConfig.Environment;
assert builtins.elem "BAR_DAEMON_YOUTUBE_METADATA=1" session.systemd.user.services.bar-daemon.serviceConfig.Environment;
assert builtins.elem "hypr-monitor-auto.service" session.systemd.user.services.bar-daemon.unitConfig.Conflicts;
assert session.systemd.user.services.hypridle.serviceConfig.Type == "notify";
assert !(helper.systemd.user.services ? bar-daemon);
assert !(location.systemd.user.services ? bar-daemon);
assert !(location.systemd.services ? location-timezone-agent);
assert !location.services.geoclue2.enable;
assert nixpkgs.lib.hasInfix "org.freedesktop.NetworkManager.wifi.scan" location.security.polkit.extraConfig;
assert !(builtins.elem self.packages.${system}.default location.services.dbus.packages);
pkgs.runCommand "bar-daemon-module-contract" { } ''
  runtime=${self.packages.${system}.default}
  test ! -e "$runtime/share/dbus-1/system-services"
  test ! -e "$runtime/share/dbus-1/system.d"
  test ! -e "$runtime/share/polkit-1"
  battery=${self.packages.${system}.batteryIntegration}
  location=${self.packages.${system}.locationIntegration}
  test -f "$battery/share/dbus-1/system-services/org.laufan.BarBatteryHelper.service"
  test -f "$battery/share/polkit-1/actions/org.laufan.bar-daemon.policy"
  test ! -e "$battery/share/dbus-1/services"
  test -f "$location/share/dbus-1/system.d/org.laufan.LocationTimezone.conf"
  test ! -e "$location/share/dbus-1/system-services"
  test ! -e "$location/share/dbus-1/services"
  touch $out
''
