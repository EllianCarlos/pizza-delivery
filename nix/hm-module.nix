self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pizza-delivery;
  toml = pkgs.formats.toml { };
in
{
  options.services.pizza-delivery = {
    enable = lib.mkEnableOption "the pizza-delivery daemon";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "pizza-delivery flake package";
      description = "The pizza-delivery package to run.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          routing.default = "claude";
        }
      '';
      description = "Contents of routing.toml, rendered to TOML under the user's XDG config directory.";
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      description = "Extra environment variables for the daemon, such as PIZZA_DELIVERY_MAX_RUNNING.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."pizza-delivery/routing.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "routing.toml" cfg.settings;
    };

    systemd.user.sockets.pizza-delivery = {
      Unit.Description = "pizza-delivery daemon socket";
      Socket.ListenStream = "%t/pizza-delivery.sock";
      Install.WantedBy = [ "sockets.target" ];
    };

    systemd.user.services.pizza-delivery = {
      Unit = {
        Description = "pizza-delivery daemon";
        Requires = [ "pizza-delivery.socket" ];
        After = [ "pizza-delivery.socket" ];
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} daemon";
        Restart = "on-failure";
        Environment = lib.mapAttrsToList (k: v: "${k}=${v}") cfg.environment;
      };
    };
  };
}
