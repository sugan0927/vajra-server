use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use std::net::TcpListener;
use std::io::{Read, Write};

pub struct CacheItem {
    pub value: Vec<u8>,
    pub expires_at: Option<Instant>,
}

pub struct ObjectCache {
    data: DashMap<Vec<u8>, CacheItem>,
    max_memory_bytes: usize,
    current_memory: AtomicUsize,
}

impl ObjectCache {
    pub fn new(max_memory_mb: usize) -> Self {
        Self {
            data: DashMap::new(),
            max_memory_bytes: max_memory_mb * 1024 * 1024,
            current_memory: AtomicUsize::new(0),
        }
    }

    pub fn start_server(self: Arc<Self>, listen_addr: String) -> std::io::Result<()> {
        let listener = TcpListener::bind(&listen_addr)?;
        println!("vajra: object_cache (Redis) listening on {}", listen_addr);

        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let cache = Arc::clone(&self);
                std::thread::spawn(move || {
                    // CHUNKING: 8KB के चंक्स में पढ़ेंगे
                    let mut buffer = Vec::new();
                    let mut chunk = [0; 8192];
                    
                    loop {
                        match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buffer.extend_from_slice(&chunk[..n]);
                                
                                // पार्सर लूप
                                while let Some((args, consumed_bytes)) = Self::parse_resp(&buffer) {
                                    let response = cache.execute_command(&args);
                                    let _ = stream.write_all(&response);
                                    
                                    // प्रोसेस्ड कमांड को बफर से हटाएँ
                                    buffer.drain(..consumed_bytes);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
        }
        Ok(())
    }

    fn parse_resp(buf: &[u8]) -> Option<(Vec<Vec<u8>>, usize)> {
        if buf.is_empty() || buf[0] != b'*' { return None; }
        
        let pos = buf.windows(2).position(|w| w == b"\r\n")?;
        let num_args = std::str::from_utf8(&buf[1..pos]).ok()?.parse::<usize>().ok()?;
        
        let mut args = Vec::with_capacity(num_args);
        let mut offset = pos + 2;
        
        for _ in 0..num_args {
            if offset >= buf.len() || buf[offset] != b'$' { return None; }
            
            let len_pos = offset + 1;
            let crlf_pos = buf[len_pos..].windows(2).position(|w| w == b"\r\n")? + len_pos;
            let len = std::str::from_utf8(&buf[len_pos..crlf_pos]).ok()?.parse::<usize>().ok()?;
            
            let data_start = crlf_pos + 2;
            let data_end = data_start + len;
            
            if data_end + 2 > buf.len() { return None; }
            
            args.push(buf[data_start..data_end].to_vec());
            offset = data_end + 2;
        }
        
        Some((args, offset))
    }

    fn execute_command(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.is_empty() { return b"-ERR empty command\r\n".to_vec(); }
        
        let cmd = String::from_utf8_lossy(&args[0]).to_uppercase();
        
        match cmd.as_str() {
            "PING" => b"+PONG\r\n".to_vec(),
            "INFO" => self.cmd_info(),
            "GET" => self.cmd_get(&args[1..]),
            "SET" => self.cmd_set(&args[1..]),
            "SETEX" => self.cmd_setex(&args[1..]),
            "EXPIRE" => self.cmd_expire(&args[1..]),
            "TTL" => self.cmd_ttl(&args[1..]),
            "DEL" | "UNLINK" => self.cmd_del(&args[1..]),
            "EXISTS" => self.cmd_exists(&args[1..]),
            "FLUSHALL" | "FLUSHDB" => {
                self.data.clear();
                self.current_memory.store(0, Ordering::SeqCst);
                b"+OK\r\n".to_vec()
            },
            _ => format!("-ERR unknown command '{}'\r\n", cmd).into_bytes(),
        }
    }

    fn cmd_get(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.is_empty() { return b"-ERR wrong number of arguments\r\n".to_vec(); }
        let key = &args[0];
        
        if let Some(item) = self.data.get(key) {
            if let Some(exp) = item.expires_at {
                if Instant::now() > exp {
                    drop(item);
                    self.data.remove(key);
                    return b"$-1\r\n".to_vec();
                }
            }
            let mut resp = format!("${}\r\n", item.value.len()).into_bytes();
            resp.extend_from_slice(&item.value);
            resp.extend_from_slice(b"\r\n");
            return resp;
        }
        b"$-1\r\n".to_vec()
    }

    fn cmd_set(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.len() < 2 { return b"-ERR wrong number of arguments\r\n".to_vec(); }
        let key = args[0].clone();
        let val = args[1].clone();
        let mut expires_at = None;

        if args.len() >= 4 {
            let opt = String::from_utf8_lossy(&args[2]).to_uppercase();
            let time_str = String::from_utf8_lossy(&args[3]);
            if let Ok(time) = time_str.parse::<u64>() {
                if opt == "EX" {
                    expires_at = Some(Instant::now() + Duration::from_secs(time));
                } else if opt == "PX" {
                    expires_at = Some(Instant::now() + Duration::from_millis(time));
                }
            }
        }

        self.insert_key(key, val, expires_at);
        b"+OK\r\n".to_vec()
    }

    fn cmd_setex(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.len() < 3 { return b"-ERR wrong number of arguments\r\n".to_vec(); }
        let key = args[0].clone();
        let time_str = String::from_utf8_lossy(&args[1]);
        let val = args[2].clone();
        
        if let Ok(time) = time_str.parse::<u64>() {
            let exp = Some(Instant::now() + Duration::from_secs(time));
            self.insert_key(key, val, exp);
            b"+OK\r\n".to_vec()
        } else {
            b"-ERR invalid expire time\r\n".to_vec()
        }
    }

    fn cmd_expire(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.len() < 2 { return b"-ERR wrong number of arguments\r\n".to_vec(); }
        let key = &args[0];
        let time_str = String::from_utf8_lossy(&args[1]);
        
        if let Ok(time) = time_str.parse::<u64>() {
            if let Some(mut item) = self.data.get_mut(key) {
                item.expires_at = Some(Instant::now() + Duration::from_secs(time));
                return b":1\r\n".to_vec();
            }
        }
        b":0\r\n".to_vec()
    }

    fn cmd_ttl(&self, args: &[Vec<u8>]) -> Vec<u8> {
        if args.is_empty() { return b"-ERR wrong number of arguments\r\n".to_vec(); }
        let key = &args[0];
        
        if let Some(item) = self.data.get(key) {
            if let Some(exp) = item.expires_at {
                let now = Instant::now();
                if now > exp { return b":-2\r\n".to_vec(); }
                let remaining = exp.duration_since(now).as_secs();
                return format!(":{}\r\n", remaining).into_bytes();
            }
            return b":-1\r\n".to_vec();
        }
        b":-2\r\n".to_vec()
    }

    fn cmd_del(&self, args: &[Vec<u8>]) -> Vec<u8> {
        let mut count = 0;
        for key in args {
            if let Some((_, old)) = self.data.remove(key) {
                self.current_memory.fetch_sub(old.value.len(), Ordering::SeqCst);
                count += 1;
            }
        }
        format!(":{}\r\n", count).into_bytes()
    }

    fn cmd_exists(&self, args: &[Vec<u8>]) -> Vec<u8> {
        let mut count = 0;
        for key in args {
            if self.data.contains_key(key) { count += 1; }
        }
        format!(":{}\r\n", count).into_bytes()
    }

    fn cmd_info(&self) -> Vec<u8> {
        let info = format!("# Server\r\nredis_version:7.0.0-vajra\r\n# Memory\r\nused_memory:{}\r\n", self.current_memory.load(Ordering::SeqCst));
        let mut resp = format!("${}\r\n", info.len()).into_bytes();
        resp.extend_from_slice(info.as_bytes());
        resp.extend_from_slice(b"\r\n");
        resp
    }

    fn insert_key(&self, key: Vec<u8>, val: Vec<u8>, expires_at: Option<Instant>) {
        let size = key.len() + val.len();
        let current = self.current_memory.load(Ordering::SeqCst);
        
        if current + size > self.max_memory_bytes {
            self.data.clear();
            self.current_memory.store(0, Ordering::SeqCst);
        }
        
        let item = CacheItem { value: val, expires_at };
        if let Some(old) = self.data.insert(key, item) {
            self.current_memory.fetch_sub(old.value.len(), Ordering::SeqCst);
        }
        self.current_memory.fetch_add(size, Ordering::SeqCst);
    }
}
