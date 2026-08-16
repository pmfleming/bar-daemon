{
  description = "Status, policy, and action daemon for a Quickshell desktop bar";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in {
      packages = forAllSystems (system: pkgs:
        let
          barDaemon = pkgs.rustPlatform.buildRustPackage {
            pname = "bar-daemon";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper pkgs.pkg-config pkgs.llvmPackages.libclang ];
            buildInputs = [ pkgs.pipewire ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
            strictDeps = true;
            postInstall = ''
              install -Dm644 ${./packaging/systemd/bar-daemon.service} $out/share/systemd/user/bar-daemon.service
              install -Dm644 ${./packaging/dbus/org.laufan.BarDaemon.service} \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service
              substituteInPlace \
                $out/share/systemd/user/bar-daemon.service \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service \
                --replace-fail @out@ $out
            '';
            meta = {
              description = "Status, policy, and action daemon for a Quickshell desktop bar";
              mainProgram = "bar-daemon";
              license = pkgs.lib.licenses.mit;
              platforms = pkgs.lib.platforms.linux;
            };
          };
        in { default = barDaemon; });

      apps = forAllSystems (system: pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/bar-daemon";
        };
      });

      checks = forAllSystems (system: pkgs: { default = self.packages.${system}.default; });
      formatter = forAllSystems (system: pkgs: pkgs.nixfmt-tree);

      devShells = forAllSystems (system: pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo clippy jq llvmPackages.libclang pkg-config pipewire rust-analyzer rustc rustfmt ];
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
          RUST_BACKTRACE = "1";
          RUST_LOG = "bar_daemon=debug";
        };
      });
    };
}
