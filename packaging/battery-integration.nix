{ pkgs, package }:
pkgs.runCommand "bar-battery-integration" { } ''
  install -Dm644 ${./systemd/bar-battery-helper.service} \
    $out/lib/systemd/system/bar-battery-helper.service
  substituteInPlace $out/lib/systemd/system/bar-battery-helper.service \
    --replace-fail @out@ ${package}
  install -Dm644 ${./dbus/org.laufan.BarBatteryHelper.service} \
    $out/share/dbus-1/system-services/org.laufan.BarBatteryHelper.service
  install -Dm644 ${./dbus/org.laufan.BarBatteryHelper.conf} \
    $out/share/dbus-1/system.d/org.laufan.BarBatteryHelper.conf
  install -Dm644 ${./polkit/org.laufan.bar-daemon.policy} \
    $out/share/polkit-1/actions/org.laufan.bar-daemon.policy
''
