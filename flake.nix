{
  description = "Dev Shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
      in
      {
        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              openssl
              pkg-config
              eza
              fd
              libclang
              cmake
              frp
              sqlite
              (rust-bin.beta.latest.default.override {
                extensions = [
                  "rust-src"
                  "rust-analyzer"
                  "clippy"
                ];
              })
              package-version-server
            ];

            shellHook = ''
              alias ls=eza
              alias find=fd
              export LIBCLANG_PATH=${libclang.lib}/lib
              # Help bindgen find C standard headers on NixOS
              export BINDGEN_EXTRA_CLANG_ARGS="-isystem ${stdenv.cc.libc.dev}/include $(< ${stdenv.cc}/nix-support/libc-cflags)"
            '';
          };
      }
    );
}
