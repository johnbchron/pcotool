{
  inputs = {
    nixpkgs.url = "https://flakehub.com/f/NixOS/nixpkgs/0.2311.556873.tar.gz";
    rust-overlay = {
      inputs.nixpkgs.follows = "nixpkgs";
      url = "https://flakehub.com/f/oxalica/rust-overlay/0.1.1327.tar.gz";
    };
    crane = {
      url = "https://flakehub.com/f/ipetkov/crane/0.16.3.tar.gz";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, crane, flake-utils }: 
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        });

        craneLib = (crane.mkLib pkgs).overrideToolchain toolchain;

        common_args = {
          src = ./.;
          doCheck = false;

          buildInputs = [ ];
          nativeBuildInputs = [ ];
        };

        deps_only = craneLib.buildDepsOnly common_args;
        crate = craneLib.buildPackage (common_args // {
          cargoArtifacts = deps_only;
        });

      in {
        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            jq bacon
            toolchain
          ];
        };
        packages = {
          default = crate;
        };
      });
}
