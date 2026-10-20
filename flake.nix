{
  description = "Status, policy, and action daemon for a Quickshell desktop bar";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  inputs.updateWorker.url = "git+file:../update-daemon";
  # Co-development: one live sibling framework; no per-daemon revision pins.
  # Use ../daemon-framework/tools/local-build for Nix builds/checks.
  inputs.daemonFramework = {
    url = "git+file:../daemon-framework";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    {
      self,
      nixpkgs,
      daemonFramework,
      updateWorker,
    }:
    let
      systems = [ "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      # Daemon-owned idle integration: one patch set for runtime and tests.
      lib.mkManagedHypridle = import ./packaging/hypridle;
      lib.mkLocationGeoclue = import ./packaging/geoclue;

      packages = forAllSystems (
        system: pkgs:
        let
          managedHypridle = self.lib.mkManagedHypridle pkgs.hypridle;
          locationGeoclue = self.lib.mkLocationGeoclue (pkgs.geoclue2.override { withDemoAgent = true; });
          barDaemon = daemonFramework.lib.buildRustPackage pkgs {
            pname = "bar-daemon";
            version = "0.1.0";
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./src
                ./tests
                ./test_support
                ./protocols
                ./packaging/licenses
                ./docs/activity.example.json
              ];
            };
            postUnpack = ''
              cp -R --no-preserve=mode ${daemonFramework.lib.daemonSource pkgs} "$(dirname "$sourceRoot")/daemon-framework"
              # The worker is a test-only dependency for real publisher/reader checks.
              cp -R --no-preserve=mode ${updateWorker} "$(dirname "$sourceRoot")/update-daemon"
            '';
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.makeWrapper
              pkgs.pkg-config
              pkgs.llvmPackages.libclang
            ];
            buildInputs = [
              pkgs.pipewire
              pkgs.systemd
            ];
            nativeCheckInputs = [
              pkgs.dbus
              pkgs.pipewire
            ];
            HYPRIDLE_TEST_BIN = "${managedHypridle}/bin/hypridle";
            LOCATION_GEOCLUE_AGENT = "${locationGeoclue}/libexec/geoclue-2.0/demos/agent";
            cargoTestFlags = [ "--all-targets" ];
            checkFlags = [ "--include-ignored" ];
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
            strictDeps = true;
            postFixup = ''
              wrapProgram $out/bin/bar-daemon --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.brightnessctl ]}
              wrapProgram $out/bin/bar-battery-helper --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.systemd ]}
              wrapProgram $out/bin/bar-location-daemon --set-default TZDIR ${pkgs.tzdata}/share/zoneinfo
            '';
            postInstall = ''
              install -Dm644 protocols/hyprland-lock-notify-v1.xml \
                $out/share/licenses/bar-daemon/hyprland-lock-notify-v1.xml
              install -Dm644 protocols/ext-idle-notify-v1.xml \
                $out/share/licenses/bar-daemon/ext-idle-notify-v1.xml
              install -Dm644 packaging/licenses/CDLA-Permissive-2.0.txt \
                $out/share/licenses/bar-daemon/CDLA-Permissive-2.0.txt
              install -Dm644 packaging/licenses/ODbL-1.0.txt $out/share/licenses/bar-daemon/ODbL-1.0.txt
              install -Dm644 packaging/licenses/timezone-boundaries.md $out/share/licenses/bar-daemon/timezone-boundaries.md
              install -Dm644 ${./packaging/systemd/bar-daemon.service} $out/share/systemd/user/bar-daemon.service
              install -Dm644 ${./packaging/dbus/org.laufan.BarDaemon.service} \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service
              install -Dm644 ${./packaging/dbus/org.freedesktop.Notifications.service} \
                $out/share/dbus-1/services/org.freedesktop.Notifications.service
              substituteInPlace \
                $out/share/systemd/user/bar-daemon.service \
                $out/share/dbus-1/services/org.laufan.BarDaemon.service \
                $out/share/dbus-1/services/org.freedesktop.Notifications.service \
                --replace-fail @out@ $out
            '';
            meta = {
              description = "Status, policy, and action daemon for a Quickshell desktop bar";
              mainProgram = "bar-daemon";
              license = with pkgs.lib.licenses; [
                mit
                odbl
              ];
              platforms = systems;
            };
          };
        in
        {
          default = barDaemon;
          batteryIntegration = import ./packaging/battery-integration.nix { inherit pkgs; package = barDaemon; };
          locationIntegration = pkgs.runCommand "bar-location-integration" { } ''
            install -Dm644 ${./packaging/dbus/org.laufan.LocationTimezone.conf} \
              $out/share/dbus-1/system.d/org.laufan.LocationTimezone.conf
            install -Dm644 ${./packaging/polkit/org.laufan.location-timezone.policy} \
              $out/share/polkit-1/actions/org.laufan.location-timezone.policy
          '';
          inherit managedHypridle locationGeoclue;
        }
      );

      apps = forAllSystems (
        system: pkgs: {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/bar-daemon";
          };
        }
      );

      nixosModules.default = import ./packaging/nixos.nix self;
      homeManagerModules.default = import ./packaging/home-manager.nix self;

      nixosModules.location-timezone = { lib, pkgs, ... }: {
        imports = [ ./packaging/location-timezone.nix ];
        services.location-timezone.package = lib.mkDefault self.packages.${pkgs.system}.default;
        services.location-timezone.integrationPackage = lib.mkDefault self.packages.${pkgs.system}.locationIntegration;
      };

      checks = forAllSystems (system: pkgs: {
        default = self.packages.${system}.default;
        modules = import ./packaging/module-check.nix { inherit self nixpkgs system pkgs; };
        locationGeoclue = self.packages.${system}.locationGeoclue;
      });
      formatter = forAllSystems (system: pkgs: pkgs.nixfmt-tree);

      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            packages =
              (daemonFramework.lib.toolchain pkgs).packages
              ++ (with pkgs; [
                brightnessctl
                cargo-audit
                cargo-deny
                cargo-llvm-cov
                cargo-mutants
                cargo-shear
                dbus
                git
                jq
                llvmPackages.libclang
                llvmPackages.llvm
                pkg-config
                pipewire
                python3
                python3Packages.diff-cover
                rust-analyzer
                systemd
              ]);
            LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
            LLVM_COV = "${pkgs.llvmPackages.llvm}/bin/llvm-cov";
            LLVM_PROFDATA = "${pkgs.llvmPackages.llvm}/bin/llvm-profdata";
            BINDGEN_EXTRA_CLANG_ARGS = "-isystem ${pkgs.stdenv.cc.libc.dev}/include";
            HYPRIDLE_TEST_BIN = "${self.packages.${system}.managedHypridle}/bin/hypridle";
            LOCATION_GEOCLUE_AGENT = "${self.packages.${system}.locationGeoclue}/libexec/geoclue-2.0/demos/agent";
            RUST_BACKTRACE = "1";
            RUST_LOG = "bar_daemon=debug";
          };
        }
      );
    };
}
