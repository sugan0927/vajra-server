# vajra wp — WP-CLI wrapper

vajra wp runs WP-CLI as the www-data user. Everything after wp is passed
to wp verbatim.

    vajra wp <args...>

## Examples

    vajra wp plugin list
    vajra wp plugin install woocommerce --activate
    vajra wp plugin activate woocommerce
    vajra wp plugin update --all
    vajra wp core version
    vajra wp core update
    vajra wp core update-db
    vajra wp db export /var/backups/site.sql
    vajra wp --path=/var/www/shop plugin update --all

## Behaviour

    - root + sudo on PATH + VAJRA_WP_NO_SUDO unset:
          sudo -u www-data -- wp <args...>
    - non-root or no sudo on PATH:
          wp <args...>
    - VAJRA_WP_NO_SUDO set (any value):
          wp <args...>  (always direct)
    - wp not on PATH:  exit 127 with a pointer to wp-cli.org
    - no args:         exit 2 with usage on stderr

## --path handling

If the caller passes --path or --path=<dir>, that wins.
Otherwise, if VAJRA_WP_PATH is set, --path=$VAJRA_WP_PATH is injected as
the first argument.
Otherwise, wp uses its own discovery (usually the cwd).

## Environment overrides

    VAJRA_WP_PATH       Injected as --path=... when the caller did not
                        pass one.
    VAJRA_WP_NO_SUDO    If set, never spawn sudo, even as root. Tests and
                        CI use this; it is also an escape hatch for
                        operators who manage privileges themselves.

## Installer

install.sh drops:

    /usr/local/bin/vajra      the server binary, which also speaks the
                              wp subcommand
    /usr/local/bin/vajra-wp   a one-line shim that execs vajra wp "$@"

So operators can use either `vajra wp ...` or `vajra-wp ...`.

## Testing

tests/wp.rs exercises the wrapper against a fake wp shell script that
appends its argv to a log file. Coverage:

    - arguments forwarded verbatim
    - VAJRA_WP_PATH injected as --path=... when absent
    - explicit --path=... wins over the env var
    - missing wp returns 127
    - db export and core update reach the fake wp unchanged

Tests serialise on a global mutex (they touch $PATH) and set
VAJRA_WP_NO_SUDO=1 so the direct-exec path is used regardless of the
runner's UID.
