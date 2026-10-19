# Add structured provenance; patch application fails on incompatible upstream changes.
package: package.overrideAttrs (old: {
  patches = (old.patches or []) ++ [ ./provenance.patch ./freshness.patch ];
  postPatch = (old.postPatch or "") + ''
    cp ${./provenance-test.c} src/provenance-test.c
  '';
  doCheck = true;
})
