# Keep readiness at the actual initialization barrier, not in ExecStartPost.
# Patch application intentionally fails on incompatible upstream changes.
package: package.overrideAttrs (old: {
  patches = (old.patches or []) ++ [ ./hypridle-control.patch ./hypridle-ready.patch ];
  postPatch = (old.postPatch or "") + ''
    cp ${./hypridle-ready.hpp} src/helpers/ShelllistReady.hpp
    cp ${./hypridle-control.cpp} src/core/ShelllistIdle.cpp
  '';
})
