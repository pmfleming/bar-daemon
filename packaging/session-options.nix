{ lib, package }:
{
  enable = lib.mkEnableOption "bar-daemon session policy and status service";
  package = lib.mkOption {
    type = lib.types.package;
    default = package;
    description = "Daemon executable and client package.";
  };
  target = lib.mkOption {
    type = lib.types.str;
    default = "graphical-session.target";
    description = "Session target that owns the daemon, independently of any UI.";
  };
  displays.enable = lib.mkEnableOption "automatic display policy (exclusive with hypr-monitor-auto)";
  media.youtubeMetadata.enable = lib.mkEnableOption "YouTube oEmbed metadata fallback";
  suspend.enable = lib.mkEnableOption "daemon-managed hypridle suspend policy";
}
