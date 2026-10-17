<?php
/**
 * Plugin Name: Vajra Loopback Fix
 * Description: Route WordPress loopback HTTP requests over plain HTTP/1.1 to 127.0.0.1,
 *              because Vajra rejects HTTP/1.0 over HTTPS (TLS close_notify), causing
 *              "cURL error 52: Empty reply from server" in Site Health, WP-Cron, REST API.
 * Version:     1.1.0
 * License:     GPL-2.0-or-later
 */

if ( ! defined( 'ABSPATH' ) ) {
    exit;
}

final class Vajra_Loopback_Fix {

    private string $host = '';
    private int    $port = 80;
    private bool   $in_loop = false;

    public function __construct() {
        $this->host = (string) wp_parse_url( home_url( '/' ), PHP_URL_HOST );
        $this->port = defined( 'VAJRA_LOOPBACK_HTTP_PORT' )
            ? (int) VAJRA_LOOPBACK_HTTP_PORT
            : 80;

        if ( $this->host ) {
            add_filter( 'pre_http_request', [ $this, 'route' ], 10, 3 );
        }
    }

    public function route( $pre, $args, $url ) {
        if ( false !== $pre || $this->in_loop || ! $this->is_loopback( $url ) ) {
            return $pre;
        }

        $this->in_loop = true;

        $new_url = preg_replace(
            '#^https?://' . preg_quote( $this->host, '#' ) . '(?::\d+)?#',
            'http://127.0.0.1:' . $this->port,
            $url
        );

        if ( ! isset( $args['headers'] ) || ! is_array( $args['headers'] ) ) {
            $args['headers'] = [];
        }
        $args['headers']['Host'] = $this->host;
        $args['sslverify']       = false;
        $args['httpversion']     = '1.1';

        $result = _wp_http_get_object()->request( $new_url, $args );

        $this->in_loop = false;
        return $result;
    }

    private function is_loopback( string $url ): bool {
        $h = strtolower( (string) wp_parse_url( $url, PHP_URL_HOST ) );
        return $h === strtolower( $this->host )
            || in_array( $h, [ '127.0.0.1', '::1', 'localhost' ], true );
    }
}

new Vajra_Loopback_Fix();
