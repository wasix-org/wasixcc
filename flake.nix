{
  description = "wasixcc with pinned WASIX LLVM, Binaryen, and sysroot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.package.version;

        isLinux = pkgs.stdenv.hostPlatform.isLinux;
        supported = isLinux && pkgs.stdenv.hostPlatform.isx86_64;

        llvmSrcForSystem =
          if system == "x86_64-linux" then
            pkgs.fetchurl {
              url = "https://github.com/wasix-org/llvm-project/releases/download/21.1.203/LLVM-Linux-x86_64.tar.gz";
              hash = "sha256-PY/diViHY2ua/Y1jcTUiyTlDARp+J0vwDsB18Rggr5Y=";
            }
          else
            throw "Unsupported system for pinned WASIX LLVM: ${system}";

        binaryenSrcForSystem =
          if system == "x86_64-linux" then
            pkgs.fetchurl {
              url = "https://github.com/WebAssembly/binaryen/releases/download/version_126/binaryen-version_126-x86_64-linux.tar.gz";
              hash = "sha256-5Ifg6sHwKmc5gWxhcnCwM+XT+MqQQ5MB/QKGRgMi/XY=";
            }
          else
            throw "Unsupported system for pinned Binaryen: ${system}";

        wasixLlvm = pkgs.stdenvNoCC.mkDerivation {
          pname = "wasix-llvm";
          version = "21.1.203";
          src = llvmSrcForSystem;

          dontUnpack = true;
          nativeBuildInputs = [ pkgs.gnutar ] ++ lib.optionals isLinux [ pkgs.autoPatchelfHook ];
          buildInputs = lib.optionals isLinux [ pkgs.stdenv.cc.cc.lib ];

          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            tar -xzf "$src" -C "$out"
            chmod -R u+w "$out"
            runHook postInstall
          '';
        };

        binaryen = pkgs.stdenvNoCC.mkDerivation {
          pname = "binaryen";
          version = "126";
          src = binaryenSrcForSystem;

          dontUnpack = true;
          nativeBuildInputs = [ pkgs.gnutar ] ++ lib.optionals isLinux [ pkgs.autoPatchelfHook ];
          buildInputs = lib.optionals isLinux [ pkgs.stdenv.cc.cc.lib ];

          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            tmp="$(mktemp -d)"
            tar -xzf "$src" -C "$tmp"
            cp -a "$tmp"/binaryen-version_126/. "$out"/
            chmod -R u+w "$out"
            runHook postInstall
          '';
        };

        wasixSysroot = pkgs.stdenvNoCC.mkDerivation {
          pname = "wasix-sysroot";
          version = "v2026-02-16.1";

          srcSysroot = pkgs.fetchurl {
            url = "https://github.com/wasix-org/wasix-libc/releases/download/v2026-02-16.1/sysroot.tar.gz";
            hash = "sha256-IUvuFhdPtPOUQU5knEXE+xwgWVmSCz7LiJ4VlzAjf+0=";
          };
          srcSysrootEh = pkgs.fetchurl {
            url = "https://github.com/wasix-org/wasix-libc/releases/download/v2026-02-16.1/sysroot-eh.tar.gz";
            hash = "sha256-0avggow76g1qp79SWjN6XbbWCK3P1A69kSYT0bWfkFQ=";
          };
          srcSysrootEhpic = pkgs.fetchurl {
            url = "https://github.com/wasix-org/wasix-libc/releases/download/v2026-02-16.1/sysroot-ehpic.tar.gz";
            hash = "sha256-6exFF8vtpUdK+tN0QFw0oZTNbRUMImZzd92g+8YfPgg=";
          };

          dontUnpack = true;
          nativeBuildInputs = [ pkgs.gnutar ];

          installPhase = ''
            runHook preInstall
            mkdir -p "$out"

            unpack_sysroot() {
              local archive="$1"
              local target="$2"
              local tmp
              tmp="$(mktemp -d)"

              tar -xzf "$archive" -C "$tmp"
              local extracted
              extracted="$(find "$tmp" -mindepth 1 -maxdepth 1 -type d | head -n 1)"

              mkdir -p "$out/$target"
              cp -a "$extracted/sysroot/." "$out/$target/"
            }

            unpack_sysroot "$srcSysroot" "sysroot"
            unpack_sysroot "$srcSysrootEh" "sysroot-eh"
            unpack_sysroot "$srcSysrootEhpic" "sysroot-ehpic"
            runHook postInstall
          '';
        };

        wasixccRaw = pkgs.rustPlatform.buildRustPackage {
          pname = "wasixcc-raw";
          inherit version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          doCheck = true;

          installPhase = ''
            runHook preInstall
            mkdir -p "$out/libexec"
            cp "$(find target -type f -path '*/release/wasixccenv' | head -n 1)" "$out/libexec/wasixccenv"
            runHook postInstall
          '';
        };

        wasixcc =
          if supported then
            pkgs.stdenvNoCC.mkDerivation {
              pname = "wasixcc";
              inherit version;
              dontUnpack = true;

              installPhase = ''
                runHook preInstall
                mkdir -p "$out/bin" "$out/libexec"

                cp "${wasixccRaw}/libexec/wasixccenv" "$out/libexec/wasixccenv"

                for cmd in wasixcc 'wasix++' wasixcc++ wasixar wasixnm wasixranlib wasixld wasixccenv; do
                  printf '%s\n' \
                    '#!${pkgs.bash}/bin/bash' \
                    'set -euo pipefail' \
                    'export WASIXCC_LLVM_LOCATION="${wasixLlvm}"' \
                    'export WASIXCC_BINARYEN_LOCATION="${binaryen}"' \
                    'export WASIXCC_SYSROOT_PREFIX="${wasixSysroot}"' \
                    "exec -a \"\$0\" \"$out/libexec/wasixccenv\" \"\$@\"" \
                    > "$out/bin/$cmd"
                  chmod +x "$out/bin/$cmd"
                done

                runHook postInstall
              '';
            }
          else
            throw "wasixcc flake package currently supports only x86_64-linux; current system is ${system}";
      in
      {
        packages = {
          default = wasixcc;
          inherit
            wasixcc
            wasixLlvm
            binaryen
            wasixSysroot
            ;
        };

        apps = {
          default = flake-utils.lib.mkApp {
            drv = wasixcc;
            name = "wasixcc";
          };
          wasixcc = flake-utils.lib.mkApp {
            drv = wasixcc;
            name = "wasixcc";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = [ wasixcc ];
        };
      }
    );
}
