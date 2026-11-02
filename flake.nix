{
  description = "Self-hosted voice and screen share, end-to-end encrypted";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, nixpkgs, fenix }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (
        pkgs:
        let
          inherit (pkgs) lib stdenv;
          toolchain = fenix.packages.${stdenv.hostPlatform.system}.fromToolchainFile {
            file = ./rust-toolchain.toml;
            sha256 = "sha256-p8h3Sl/YRByZfZTAKXdsvF6xEenXKrXSVvpphmZENH4=";
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };

          ffmpeg = stdenv.mkDerivation {
            pname = "ffmpeg-lgpl";
            version = "8.1.3";
            src = lib.fileset.toSource {
              root = ./packaging/ffmpeg;
              fileset = ./packaging/ffmpeg;
            };
            tarball = pkgs.fetchurl {
              url = "https://ffmpeg.org/releases/ffmpeg-8.1.3.tar.xz";
              hash = "sha256-cTjSjJbZ0+OvTuPYytcnQfj/tA2pDBESI13qPs0xeKM=";
            };
            nativeBuildInputs =
              with pkgs;
              [
                nasm
                pkg-config
                rust-bindgen
              ]
              # ffmpeg's configure calls the C compiler gcc, which Xcode provides on macOS.
              ++ lib.optionals stdenv.hostPlatform.isDarwin [ (writeShellScriptBin "gcc" ''exec cc "$@"'') ];
            buildInputs =
              with pkgs;
              [
                dav1d
                libvpx
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [
                libva
                nv-codec-headers-13
              ];
            dontConfigure = true;
            buildPhase = ''
              patchShebangs build.sh bindings.sh
              FFMPEG_TARBALL=$tarball ./build.sh $out
              ./bindings.sh $out $out/binding.rs
            '';
            dontInstall = true;
          };

          # The ScreenCaptureKit bridges are Swift packages built with the system's Xcode
          # command line tools, which need their own SDK rather than nixpkgs'.
          # SwiftPM can't nest its manifest sandbox inside a nix build, so the shim turns it off.
          # The binary links with Apple's clang too, against the Swift runtime the bridges target.
          xcode = pkgs.runCommand "xcode-tools" { } ''
            mkdir -p $out/bin
            cat >$out/bin/apple-cc <<'EOF'
            #!/bin/sh
            exec env -u SDKROOT -u DEVELOPER_DIR /usr/bin/cc "$@"
            EOF
            cat >$out/bin/xcrun <<'EOF'
            #!/bin/sh
            exec env -u SDKROOT -u DEVELOPER_DIR /usr/bin/xcrun "$@"
            EOF
            cat >$out/bin/swift <<'EOF'
            #!/bin/sh
            export HOME=$TMPDIR
            if [ "$1" = build ]; then
              shift
              set -- build --disable-sandbox "$@"
            fi
            exec env -u SDKROOT -u DEVELOPER_DIR /usr/bin/swift "$@"
            EOF
            chmod +x $out/bin/*
          '';

          linuxRuntime = with pkgs; [
            libxkbcommon
            vulkan-loader
            wayland
            xorg.libX11
            xorg.libXcursor
            xorg.libXi
            xorg.libxcb
          ];

          workspace =
            {
              pname,
              package,
              features ? [ ],
              buildInputs ? [ ],
              nativeBuildInputs ? [ ],
              env ? { },
              postInstall ? "",
            }:
            rustPlatform.buildRustPackage {
              inherit
                pname
                buildInputs
                env
                postInstall
                ;
              version = "0.1.0";
              src = self;
              cargoLock.lockFile = ./Cargo.lock;
              cargoBuildFlags = [
                "-p"
                package
              ]
              ++ lib.optionals (features != [ ]) [
                "--features"
                (lib.concatStringsSep "," features)
              ];
              nativeBuildInputs = [ pkgs.pkg-config ] ++ nativeBuildInputs;
              doCheck = false;
              meta.mainProgram = package;
            };
        in
        rec {
          default = client;

          client = workspace {
            pname = "throcc";
            package = "throcc-client";
            features = [ "ffmpeg" ];
            nativeBuildInputs =
              [ pkgs.rustPlatform.bindgenHook ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [ pkgs.makeWrapper ]
              ++ lib.optionals stdenv.hostPlatform.isDarwin [ xcode ];
            buildInputs =
              with pkgs;
              lib.optionals stdenv.hostPlatform.isLinux (
                [
                  alsa-lib
                  dbus
                  fontconfig
                  freetype
                  libva
                  pipewire
                ]
                ++ linuxRuntime
              );
            env = {
              FFMPEG_DLL_PATH = "${ffmpeg}/lib";
              FFMPEG_BINDING_PATH = "${ffmpeg}/binding.rs";
            }
            // lib.optionalAttrs stdenv.hostPlatform.isDarwin {
              # RUSTFLAGS replaces .cargo/config.toml's rustflags, so this repeats its Swift rpath.
              RUSTFLAGS = "-C linker=${xcode}/bin/apple-cc -C link-arg=-Wl,-rpath,/usr/lib/swift";
            };
            postInstall = lib.optionalString stdenv.hostPlatform.isLinux ''
              wrapProgram $out/bin/throcc-client \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath linuxRuntime}
            '';
          };

          server = workspace {
            pname = "throcc-server";
            package = "throcc-server";
          };
        }
      );

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.client ];
        };
      });
    };
}
