# Compression - design

Brotli on-the-fly compression. Default off; enabled from [compression].

## Config

    [compression]
    brotli   = true
    level    = 6
    min_size = 1024

level > 11 is fatal; min_size = 0 warns.

## Gates

Compress iff all: enabled; not HEAD; len >= min_size; CT not pre-compressed;
Accept-Encoding negotiates br.

## Skip list

image/* except svg+xml; video/*; audio/*; zip gzip zstd rar 7z bzip2 pdf;
font woff woff2 ttf otf; ms-fontobject; x-font-ttf; x-font-otf.

## Response shape

Content-Encoding: br; Vary: Accept-Encoding; Content-Length = compressed size.

## HEAD

Skip; Content-Length stays uncompressed.

## Metrics

Not in Day 15.
