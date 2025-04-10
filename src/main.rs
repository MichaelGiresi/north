use std::net::{TcpListener, TcpStream, SocketAddrV4, IpAddr, Ipv4Addr};
use std::io::{self, Write, BufReader, BufRead, stdout};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::collections::HashSet;
use std::env;
use std::process::Command;
use igd::{PortMappingProtocol, SearchOptions};
use if_addrs;
use hostname;

struct P2PNetwork {
    peers: Arc<Mutex<HashSet<String>>>,
    potential_peers: Arc<Mutex<HashSet<String>>>,
    local_addr: String,
    peer_addr: String,
    running: Arc<Mutex<bool>>,
    connected: Arc<Mutex<bool>>, // Reintroduced for reliable connection
}

impl P2PNetwork {
    fn new(local_addr: &str, peer_addr: &str) -> Self {
        let mut potential_peers = HashSet::new();
        if !peer_addr.is_empty() {
            potential_peers.insert(peer_addr.to_string());
        }
        P2PNetwork {
            peers: Arc::new(Mutex::new(HashSet::new())),
            potential_peers: Arc::new(Mutex::new(potential_peers)),
            local_addr: local_addr.to_string(),
            peer_addr: peer_addr.to_string(),
            running: Arc::new(Mutex::new(true)),
            connected: Arc::new(Mutex::new(false)),
        }
    }

    fn start(&self) -> Result<(thread::JoinHandle<()>, thread::JoinHandle<()>), String> {
        let port = self.local_addr.split(':').last().unwrap().parse::<u16>()
            .map_err(|_| "Invalid port number".to_string())?;
        let bind_addr = format!("0.0.0.0:{}", port);

        configure_firewall(port);

        let local_ip = get_local_ip().unwrap_or(Ipv4Addr::new(0, 0, 0, 0));
        if !self.local_addr.contains("srv787206") {
            if let Ok(gateway) = igd::search_gateway(SearchOptions::default()) {
                let local_addr = SocketAddrV4::new(local_ip, port);
                let _ = gateway.add_port(PortMappingProtocol::TCP, port, local_addr, 3600, "P2P Network");
            }
        }

        let listener = TcpListener::bind(&bind_addr)
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;

        let peers = Arc::clone(&self.peers);
        let running = Arc::clone(&self.running);
        let connected = Arc::clone(&self.connected);
        let peer_addr = self.peer_addr.clone();

        let listener_handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if !*running.lock().unwrap() {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let peers_clone = Arc::clone(&peers);
                        let connected_clone = Arc::clone(&connected);
                        let peer_addr_clone = peer_addr.clone();
                        thread::spawn(move || {
                            Self::handle_connection(stream, peers_clone, connected_clone, &peer_addr_clone);
                        });
                    }
                    Err(_) => {}
                }
            }
        });

        let discovery_peers = Arc::clone(&self.peers);
        let discovery_potential = Arc::clone(&self.potential_peers);
        let discovery_running = Arc::clone(&self.running);
        let discovery_connected = Arc::clone(&self.connected);
        let local_addr = self.local_addr.clone();
        let peer_addr = self.peer_addr.clone();

        let discovery_handle = thread::spawn(move || {
            while *discovery_running.lock().unwrap() {
                if *discovery_connected.lock().unwrap() {
                    break;
                }
                let potential = discovery_potential.lock().unwrap().clone();
                for peer_addr in potential {
                    if !discovery_peers.lock().unwrap().contains(&peer_addr) {
                        if let Ok(mut stream) = TcpStream::connect_timeout(&peer_addr.parse().unwrap(), Duration::from_secs(5)) {
                            let message = format!("HELLO from {}", local_addr);
                            if stream.write_all(message.as_bytes()).is_ok() {
                                stream.flush().unwrap();
                                let mut reader = BufReader::new(&stream);
                                let mut buffer = String::new();
                                stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                                if reader.read_line(&mut buffer).is_ok() && buffer.trim().starts_with("HELLO") {
                                    discovery_peers.lock().unwrap().insert(peer_addr.clone());
                                    *discovery_connected.lock().unwrap() = true;
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_secs(5));
            }
        });

        Ok((listener_handle, discovery_handle))
    }

    fn handle_connection(stream: TcpStream, peers: Arc<Mutex<HashSet<String>>>, connected: Arc<Mutex<bool>>, expected_peer: &str) {
        let peer_addr = stream.peer_addr().unwrap().to_string();
        let peer_ip = peer_addr.split(':').next().unwrap();
        let expected_ip = expected_peer.split(':').next().unwrap();
        {
            let mut peers_guard = peers.lock().unwrap();
            peers_guard.insert(peer_addr.clone());
            if peer_ip == expected_ip && !*connected.lock().unwrap() {
                println!("Connected to your friend!");
                stdout().flush().unwrap();
                *connected.lock().unwrap() = true;
            }
        }
        
        let mut reader = BufReader::new(&stream);
        let mut buffer = String::new();
        
        let message = format!("HELLO from {}", expected_peer);
        if let Ok(mut writer) = stream.try_clone() {
            writer.write_all(message.as_bytes()).unwrap();
            writer.flush().unwrap();
        }

        while let Ok(bytes_read) = reader.read_line(&mut buffer) {
            if bytes_read == 0 {
                break;
            }
            let message = buffer.trim();
            if !message.starts_with("HELLO") {
                println!("<friend> {}", message);
                stdout().flush().unwrap();
                print_prompt();
            }
            buffer.clear();
        }
        peers.lock().unwrap().remove(&peer_addr);
    }

    fn send_message(&self, message: &str) {
        if let Ok(mut stream) = TcpStream::connect(&self.peer_addr) {
            let full_message = format!("{}\n", message);
            if stream.write_all(full_message.as_bytes()).is_ok() {
                stream.flush().unwrap();
            }
        }
    }

    fn shutdown(&self) {
        *self.running.lock().unwrap() = false;
    }

    fn wait_for_connection(&self) {
        println!("Connecting to your friend...");
        stdout().flush().unwrap();
        while !*self.connected.lock().unwrap() {
            thread::sleep(Duration::from_secs(1));
        }
    }

    fn chat_mode(&self) {
        println!("Chatting with your friend (type 'exit' to go back):");
        stdout().flush().unwrap();
        print_prompt();
        loop {
            let mut message = String::new();
            io::stdin().read_line(&mut message).expect("Failed to read message");
            let message = message.trim();
            if message.eq_ignore_ascii_case("exit") {
                println!("Going back to the menu...");
                stdout().flush().unwrap();
                break;
            }
            if !message.is_empty() {
                self.send_message(message);
                print_prompt();
            }
        }
    }

    fn run_menu(&self) {
        loop {
            println!("You’re connected! What would you like to do?");
            println!("1. Chat with your friend");
            println!("2. Say goodbye");
            stdout().flush().unwrap();
            print_prompt();

            let mut choice = String::new();
            io::stdin().read_line(&mut choice).expect("Failed to read choice");
            let choice = choice.trim();

            match choice {
                "1" => self.chat_mode(),
                "2" => {
                    println!("Goodbye!");
                    stdout().flush().unwrap();
                    self.shutdown();
                    break;
                }
                _ => {
                    println!("Please type 1 or 2.");
                    stdout().flush().unwrap();
                }
            }
        }
    }
}

fn print_prompt() {
    print!("> ");
    stdout().flush().unwrap();
}

fn get_local_ip() -> Option<Ipv4Addr> {
    for interface in if_addrs::get_if_addrs().unwrap_or_default() {
        if !interface.is_loopback() && interface.ip().is_ipv4() {
            if let IpAddr::V4(ipv4) = interface.ip() {
                return Some(ipv4);
            }
        }
    }
    None
}

fn configure_firewall(port: u16) {
    let os = env::consts::OS;
    match os {
        "windows" => {
            let _ = Command::new("netsh")
                .args(&[
                    "advfirewall",
                    "firewall",
                    "add",
                    "rule",
                    &format!("name=North P2P Network"),
                    "dir=in",
                    "action=allow",
                    "protocol=TCP",
                    &format!("localport={}", port),
                ])
                .status();
        }
        "linux" => {
            if Command::new("ufw").arg("status").output().is_ok() {
                let _ = Command::new("ufw")
                    .args(&["allow", &format!("{}/tcp", port)])
                    .status();
            }
        }
        _ => {}
    }
}

fn determine_node_config() -> (String, String) {
    const NODE_A_PUBLIC: &str = "47.17.52.8";
    const NODE_B_PUBLIC: &str = "82.25.86.57";
    const PORT: u16 = 8000;

    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("node-a") => (
            format!("{}:{}", NODE_A_PUBLIC, PORT),
            format!("{}:{}", NODE_B_PUBLIC, PORT),
        ),
        Some("node-b") => (
            format!("{}:{}", NODE_B_PUBLIC, PORT),
            format!("{}:{}", NODE_A_PUBLIC, PORT),
        ),
        _ => {
            if let Ok(hostname) = hostname::get() {
                if hostname.to_string_lossy().contains("srv787206") {
                    (
                        format!("{}:{}", NODE_B_PUBLIC, PORT),
                        format!("{}:{}", NODE_A_PUBLIC, PORT),
                    )
                } else {
                    (
                        format!("{}:{}", NODE_A_PUBLIC, PORT),
                        format!("{}:{}", NODE_B_PUBLIC, PORT),
                    )
                }
            } else {
                (
                    format!("{}:{}", NODE_A_PUBLIC, PORT),
                    format!("{}:{}", NODE_B_PUBLIC, PORT),
                )
            }
        }
    }
}

fn main() {
    let (local_addr, peer_addr) = determine_node_config();
    let network = P2PNetwork::new(&local_addr, &peer_addr);
    let (listener_handle, discovery_handle) = match network.start() {
        Ok(handles) => handles,
        Err(_) => {
            println!("Oops! Something went wrong. Try again later.");
            stdout().flush().unwrap();
            return;
        }
    };

    network.wait_for_connection();
    network.run_menu();

    network.shutdown();
    listener_handle.join().expect("Listener thread panicked");
    discovery_handle.join().expect("Discovery thread panicked");
}