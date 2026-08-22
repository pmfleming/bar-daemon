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
            buildInputs = [ pkgs.pipewire pkgs.systemd ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
            strictDeps = true;
            postFixup = ''
              wrapProgram $out/bin/bar-daemon --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.brightnessctl ]}
            '';
            postInstall = ''
              install -Dm644 ${./packaging/systemd/bar-daemon.service} $out/share/systemd/user/bar-daemon.service
              install -Dm644 ${./packaging/dbus/org.laufan.BarDaemon.service} \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service
              install -Dm644 ${./packaging/dbus/org.freedesktop.Notifications.service} \
                $out/share/dbus-1/services/org.freedesktop.Notifications.service
              install -Dm644 ${./packaging/systemd/bar-battery-helper.service} \
                $out/lib/systemd/system/bar-battery-helper.service
              install -Dm644 ${./packaging/dbus/org.laufan.BarBatteryHelper.service} \
                $out/share/dbus-1/system-services/org.laufan.BarBatteryHelper.service
              install -Dm644 ${./packaging/dbus/org.laufan.BarBatteryHelper.conf} \
                $out/share/dbus-1/system.d/org.laufan.BarBatteryHelper.conf
              install -Dm644 ${./packaging/polkit/org.laufan.bar-daemon.policy} \
                $out/share/polkit-1/actions/org.laufan.bar-daemon.policy
              substituteInPlace \
                $out/share/systemd/user/bar-daemon.service \
                $out/lib/systemd/system/bar-battery-helper.service \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service \
                $out/share/dbus-1/services/org.freedesktop.Notifications.service \
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

      nixosModules.default = { config, lib, pkgs, ... }:
        let cfg = config.services.bar-daemon;
        in {
          options.services.bar-daemon.enable = lib.mkEnableOption "bar-daemon services";
          config = lib.mkIf cfg.enable {
            environment.systemPackages = [ self.packages.${pkgs.system}.default ];
            services.dbus.packages = [ self.packages.${pkgs.system}.default ];
            security.polkit.enable = true;
            systemd.packages = [ self.packages.${pkgs.system}.default ];
          };
        };

      checks = forAllSystems (system: pkgs: { default = self.packages.${system}.default; });
      formatter = forAllSystems (system: pkgs: pkgs.nixfmt-tree);

      devShells = forAllSystems (system: pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            brightnessctl
            cargo
            cargo-llvm-cov
            clippy
            jq
            llvmPackages.libclang
            llvmPackages.llvm
            pkg-config
            pipewire
            python3Packages.diff-cover
            rust-analyzer
            rustc
            rustfmt
            systemd
          ];
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
          LLVM_COV = "${pkgs.llvmPackages.llvm}/bin/llvm-cov";
          LLVM_PROFDATA = "${pkgs.llvmPackages.llvm}/bin/llvm-profdata";
          BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
          RUST_BACKTRACE = "1";
          RUST_LOG = "bar_daemon=debug";
        };
      });
    };
}
