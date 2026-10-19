{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.location-timezone;
  geoclueAgent = config.services.geoclue2.package.override { withDemoAgent = true; };
in
{
  options.services.location-timezone = {
    enable = lib.mkEnableOption "GeoClue-first timezone authority with conditional IP confirmation";
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
    services.geoclue2 = {
      enable = true;
      enableDemoAgent = lib.mkDefault false;
      # The dedicated service-account agent still registers under this ID.
      whitelistedAgents = [ "geoclue-demo-agent" ];
      package = lib.mkDefault ((import ./geoclue) pkgs.geoclue2);
      enableNmea = lib.mkDefault false;
      geoProviderUrl = lib.mkDefault "https://api.beacondb.net/v1/geolocate";
      appConfig.location-timezone = {
        isAllowed = true;
        isSystem = true;
        users = [ (toString config.ids.uids.automatic-timezoned) ];
      };
    };
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
        if (action.id == "org.freedesktop.timedate1.set-timezone"
            && subject.user == "automatic-timezoned") {
          return polkit.Result.YES;
        }
      });
    '';
    # GeoClue 2.7 requires an agent associated with the requesting UID even
    # for explicitly allowed system applications. This agent cannot set timezones.
    systemd.services.location-timezone-agent = {
      description = "GeoClue authorization agent for the timezone authority";
      requires = [ "geoclue.service" ];
      after = [ "geoclue.service" ];
      serviceConfig = {
        User = "automatic-timezoned";
        ExecStart = "${geoclueAgent}/libexec/geoclue-2.0/demos/agent";
        Restart = "on-failure";
        PrivateTmp = true;
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateNetwork = true;
        RestrictAddressFamilies = [ "AF_UNIX" ];
      };
    };
    # Same unit replaces the old Python ExecStart: no second timezone writer.
    systemd.services.location-timezone = {
      description = "GeoClue-first timezone authority with conditional IP confirmation";
      wantedBy = [ "multi-user.target" ];
      requires = [ "location-timezone-agent.service" ];
      conflicts = [ "automatic-timezoned.service" ];
      after = [
        "automatic-timezoned.service"
        "NetworkManager.service"
        "location-timezone-agent.service"
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
