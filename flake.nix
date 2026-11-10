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
          inherit (stdenv.hostPlatform) isLinux isDarwin;
          toolchain = fenix.packages.${stdenv.hostPlatform.system}.fromToolchainFile {
            file = ./rust-toolchain.toml;
            sha256 = "sha256-p8h3Sl/YRByZfZTAKXdsvF6xEenXKrXSVvpphmZENH4=";
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };

          workspaceVersion = (lib.importTOML ./Cargo.toml).workspace.package.version;
          versionOf =
            crate:
            let
              version = (lib.importTOML ./crates/${crate}/Cargo.toml).package.version;
            in
            if lib.isString version then version else workspaceVersion;
          workspaceSrc = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./.cargo
              ./crates
              ./third_party
            ];
          };

          ffmpegScript = builtins.readFile ./packaging/ffmpeg/build.sh;
          ffmpegScriptVar = name: builtins.head (builtins.match ".*\n${name}=([^\n]+)\n.*" ffmpegScript);

          ffmpeg = stdenv.mkDerivation rec {
            pname = "ffmpeg-lgpl";
            version = ffmpegScriptVar "version";
            src = lib.fileset.toSource {
              root = ./packaging/ffmpeg;
              fileset = ./packaging/ffmpeg;
            };
            tarball = pkgs.fetchurl {
              url = "https://ffmpeg.org/releases/ffmpeg-${version}.tar.xz";
              sha256 = ffmpegScriptVar "sha256";
            };
            nativeBuildInputs =
              with pkgs;
              [
                nasm
                pkg-config
                rust-bindgen
              ]
              # ffmpeg's configure calls the C compiler gcc, which Xcode provides on macOS.
              ++ lib.optionals isDarwin [ (writeShellScriptBin "gcc" ''exec cc "$@"'') ];
            buildInputs =
              with pkgs;
              [
                dav1d
                libvpx
              ]
              ++ lib.optionals isLinux [
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

          crate =
            {
              package,
              pname ? package,
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
                postInstall
                ;
              env =
                lib.optionalAttrs (stdenv.hostPlatform.system == "x86_64-linux") {
                  # Rust's bundled lld, the default linker on this target, bypasses the nixpkgs
                  # linker wrapper, which adds the store interpreter and rpath.
                  RUSTFLAGS = "-C linker-features=-lld";
                }
                // env;
              version = versionOf package;
              src = workspaceSrc;
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

          # Everything that builds throcc-client-core: audio, capture and the keyring.
          clientCore =
            args:
            crate (
              args
              // {
                nativeBuildInputs = [
                  pkgs.rustPlatform.bindgenHook
                ]
                ++ lib.optionals isDarwin [ xcode ]
                ++ args.nativeBuildInputs or [ ];
                buildInputs =
                  lib.optionals isLinux (
                    with pkgs;
                    [
                      alsa-lib
                      dbus
                      pipewire
                    ]
                  )
                  ++ args.buildInputs or [ ];
                env =
                  lib.optionalAttrs isDarwin {
                    # RUSTFLAGS replaces .cargo/config.toml's rustflags, so this repeats its Swift rpath.
                    RUSTFLAGS = "-C linker=${xcode}/bin/apple-cc -C link-arg=-Wl,-rpath,/usr/lib/swift";
                  }
                  // args.env or { };
              }
            );
        in
        rec {
          default = client;

          client = clientCore {
            pname = "throcc";
            package = "throcc-client";
            features = [ "ffmpeg" ];
            nativeBuildInputs = lib.optionals isLinux [ pkgs.makeWrapper ];
            buildInputs = lib.optionals isLinux (
              with pkgs;
              [
                fontconfig
                freetype
                libva
              ]
              ++ linuxRuntime
            );
            env = {
              FFMPEG_DLL_PATH = "${ffmpeg}/lib";
              FFMPEG_BINDING_PATH = "${ffmpeg}/binding.rs";
            };
            postInstall = lib.optionalString isLinux ''
              wrapProgram $out/bin/throcc-client \
                --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath linuxRuntime}
            '';
          };

          cli = clientCore {
            package = "throcc-cli";
          };

          server = crate {
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
