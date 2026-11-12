{
  lib,
  stdenvNoCC,
  bun,
  craneLib,
  installShellFiles,
  stdenv,
  nodeModulesHash,
}:
let
  src = lib.fileset.toSource {
    root = ./..;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };

  webSrc = lib.fileset.toSource {
    root = ../web;
    fileset = lib.fileset.difference ../web (lib.fileset.maybeMissing ../web/node_modules);
  };

  nodeModules = stdenvNoCC.mkDerivation {
    name = "pizza-delivery-web-node-modules";
    src = webSrc;
    nativeBuildInputs = [ bun ];
    dontConfigure = true;
    dontFixup = true;
    buildPhase = ''
      export HOME=$TMPDIR
      bun install --frozen-lockfile --no-progress
    '';
    installPhase = ''
      cp -r node_modules $out
    '';
    outputHashMode = "recursive";
    outputHash = nodeModulesHash;
  };

  web = stdenvNoCC.mkDerivation {
    name = "pizza-delivery-web";
    src = webSrc;
    nativeBuildInputs = [ bun ];
    dontConfigure = true;
    buildPhase = ''
      export HOME=$TMPDIR
      cp -r ${nodeModules} node_modules
      chmod -R u+w node_modules
      bun run build
    '';
    installPhase = ''
      cp -r dist $out
    '';
  };

  commonArgs = {
    pname = "pizza-delivery";
    strictDeps = true;
    inherit src;
    cargoExtraArgs = "-p pizza-delivery";
    doCheck = false;
  };
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    nativeBuildInputs = [ installShellFiles ];
    preBuild = ''
      mkdir -p web
      rm -rf web/dist
      cp -r ${web} web/dist
    '';
    postInstall = lib.optionalString (stdenv.buildPlatform.canExecute stdenv.hostPlatform) ''
      for shell in bash zsh fish; do
        $out/bin/pizza-delivery completions $shell > pizza-delivery.$shell
      done
      installShellCompletion \
        --bash --name pizza-delivery pizza-delivery.bash \
        --zsh --name _pizza-delivery pizza-delivery.zsh \
        --fish --name pizza-delivery.fish pizza-delivery.fish
    '';
    meta = {
      description = "Autonomous prompt queue that ends at a draft PR";
      license = [
        lib.licenses.mit
        lib.licenses.asl20
      ];
      mainProgram = "pizza-delivery";
    };
  }
)
