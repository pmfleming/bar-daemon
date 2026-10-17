{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.location-timezone;
in
{
  options.services.location-timezone = {
    enable = lib.mkEnableOption "Rust Wi-Fi/IP timezone consensus daemon";
    package = lib.mkOption {
      type = lib.types.package;
      description = "bar-daemon package providing bar-location-daemon";
    };
    integrationPackage = lib.mkOption {
      type = lib.types.package;
      description = "Location-only D-Bus and read-access polkit policies, without unrelated services.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !config.services.automatic-timezoned.enable;
        message = "location-timezone and automatic-timezoned must not run together";
      }
      {
        assertion = config.networking.networkmanager.enable;
        message = "location-timezone requires NetworkManager";
      }
    ];
    time.timeZone = null;
    # This authority owns filtered NetworkManager -> beaconDB acquisition.
    # Do not enable, disable or configure GeoClue for unrelated applications.
    # Preserve the deployed account and state ownership during Python -> Rust migration.
    users.users.automatic-timezoned = {
      isSystemUser = true;
      uid = config.ids.uids.automatic-timezoned;
      group = "automatic-timezoned";
    };
    users.groups.automatic-timezoned.gid = config.ids.gids.automatic-timezoned;
    services.dbus.packages = [ cfg.integrationPackage ];
    security.polkit.enable = true;
    environment.systemPackages = [ cfg.integrationPackage ];
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if ((action.id == "org.freedesktop.timedate1.set-timezone"
             || action.id == "org.freedesktop.NetworkManager.wifi.scan")
            && subject.user == "automatic-timezoned") {
          return polkit.Result.YES;
        }
      });
    '';
    # Same unit replaces the old Python ExecStart: no second timezone writer.
    systemd.services.location-timezone = {
      description = "Rust location authority with fresh Wi-Fi/IP timezone consensus";
      wantedBy = [ "multi-user.target" ];
      conflicts = [ "automatic-timezoned.service" ];
      after = [
        "automatic-timezoned.service"
        "NetworkManager.service"
      ];
      environment.TZDIR = "${pkgs.tzdata}/share/zoneinfo";
      serviceConfig = {
        Type = "exec";
        User = "automatic-timezoned";
        ExecStart = "${cfg.package}/bin/bar-location-daemon";
        ExecReload = "${pkgs.coreutils}/bin/kill -HUP $MAINPID";
        Restart = "on-failure";
        RestartSec = 10;
        TimeoutStopSec = 25;
        StateDirectory = "location-timezone";
        StateDirectoryMode = "0700";
        RuntimeDirectory = "location-timezone";
        RuntimeDirectoryMode = "0755";
        UMask = "0077";
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [
          "AF_UNIX"
          "AF_INET"
          "AF_INET6"
        ];
      };
    };
  };
}
