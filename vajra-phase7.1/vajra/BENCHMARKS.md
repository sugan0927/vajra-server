# Vajra Server Benchmarks 📊

**Server Specifications:**
- **OS:** Debian 13 (x86_64)
- **Kernel:** 6.12.113
- **Vajra Version:** v0.15.11
- **Architecture:** Rust + `io_uring` + Zero-Copy `splice` (1MB Buffer)
- **Protocol:** HTTP/2 + TLS 1.3

---

## 1. Static File Serving (Zero-Copy `splice` Test)
This test measures the raw throughput of Vajra delivering a 10MB static file.

- **File Size:** 10 MB
- **Concurrent Connections:** 100
- **Threads:** 4
- **Tool:** `wrk -t4 -c100 -d10s --latency`

### Results:
```text
Running 10s test @ https://wp.aidoor.co.in/benchmark_10mb.bin
  4 threads and 100 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     2.40s    91.58ms   2.53s    75.13%
    Req/Sec    41.30     44.53   181.00     77.14%
  Latency Distribution
     50%    2.43s 
     75%    2.47s 
     90%    2.47s 
     99%    2.53s 
  398 requests in 10.10s, 4.02GB read
  Socket errors: connect 106, read 0, write 0, timeout 0
Requests/sec:     39.42
Transfer/sec:    407.21MB
2. Dynamic Content (FastCGI / WordPress Test)
This test measures the performance of Vajra handling dynamic requests proxied to PHP-FPM.

URL: /wp-json/ (WordPress REST API)

Concurrent Connections: 100

Threads: 4

Tool: wrk -t4 -c100 -d10s --latency

Results:
Running 10s test @ https://wp.aidoor.co.in/wp-json/
  4 threads and 100 connections
  Thread Stats   Avg      Stdev     Max   +/- Stdev
    Latency     2.50s   448.48ms   2.87s    85.88%
    Req/Sec    13.33     10.39    50.00     74.47%
  Latency Distribution
     50%    2.66s 
     75%    2.75s 
     90%    2.80s 
     99%    2.84s 
  340 requests in 10.08s, 64.17MB read
  Socket errors: connect 97, read 0, write 0, timeout 0
Requests/sec:     33.72
Transfer/sec:      6.36MB
Benchmarks generated automatically on Vajra Server.
