use std::net::{TcpListener, TcpStream, SocketAddrV4, IpAddr, Ipv4Addr};
use std::io::{self, Write, BufReader, BufRead, stdout};
use std::sync::{Arc, Mutex, Condvar};
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
    connected: Arc<(Mutex<bool>, Condvar)>,
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
            connected: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn start(&self) -> Result<(thread::JoinHandle<()>, thread::JoinHandle<()>), String> {
        let port = self.local_addr.split(':').last().unwrap().parse::<u16>()
            .map_err(|_| "Invalid port number".to_string())?;
        let bind_addr = format!("0.0.0.0:{}", port);

        configure_firewall(port);

        let local_ip = get_local_ip().unwrap_or_else(|| {
            println!("Failed to determine local IP, falling back to 0.0.0.0");
            stdout().flush().unwrap();
            Ipv4Addr::new(0, 0, 0, 0)
        });
        println!("Using local IP for UPnP: {}", local_ip);
        stdout().flush().unwrap();

        if !self.local_addr.contains("srv787206") {
            match igd::search_gateway(SearchOptions::default()) {
                Ok(gateway) => {
                    let local_addr = SocketAddrV4::new(local_ip, port);
                    match gateway.get_external_ip() {
                        Ok(external_ip) => println!("Requesting UPnP mapping: {} -> {}:{}", external_ip, local_ip, port),
                        Err(e) => println!("Failed to get external IP: {}", e),
                    }
                    stdout().flush().unwrap();
                    match gateway.add_port(PortMappingProtocol::TCP, port, local_addr, 3600, "P2P Network") {
                        Ok(()) => println!("UPnP port forwarding set up for port {}", port),
                        Err(e) => println!("Failed to set up UPnP: {}. Manual port forwarding may be required.", e),
                    }
                    stdout().flush().unwrap();
                }
                Err(e) => {
                    println!("UPnP gateway not found: {}. Manual port forwarding may be required.", e);
                    stdout().flush().unwrap();
                }
            }
        } else {
            println!("Running on VPS, skipping UPnP. Ensure port forwarding is set up manually.");
            stdout().flush().unwrap();
        }

        let listener = TcpListener::bind(&bind_addr)
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;
        println!("Node started at {}", self.local_addr);
        stdout().flush().unwrap();

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
                    Err(e) => {
                        println!("Error accepting connection: {}", e);
                        stdout().flush().unwrap();
                    }
                }
            }
            println!("Listener shut down");
            stdout().flush().unwrap();
        });

        let discovery_peers = Arc::clone(&self.peers);
        let discovery_potential = Arc::clone(&self.potential_peers);
        let discovery_running = Arc::clone(&self.running);
        let discovery_connected = Arc::clone(&self.connected);
        let local_addr = self.local_addr.clone();
        let peer_addr = self.peer_addr.clone();

        let discovery_handle = thread::spawn(move || {
            while *discovery_running.lock().unwrap() {
                let connected = *discovery_connected.0.lock().unwrap();
                if connected {
                    println!("Peer {} already connected, stopping discovery attempts", peer_addr);
                    stdout().flush().unwrap();
                    break;
                }
                let potential = discovery_potential.lock().unwrap().clone();
                for peer_addr in potential {
                    if !discovery_peers.lock().unwrap().contains(&peer_addr) {
                        println!("Attempting to connect to peer {}", peer_addr);
                        stdout().flush().unwrap();
                        match TcpStream::connect_timeout(&peer_addr.parse().unwrap(), Duration::from_secs(5)) {
                            Ok(mut stream) => {
                                println!("Connection established to {}", peer_addr);
                                stdout().flush().unwrap();
                                let message = format!("HELLO from {}", local_addr);
                                if stream.write_all(message.as_bytes()).is_ok() {
                                    stream.flush().unwrap();
                                    println!("Sent HELLO to {}", peer_addr);
                                    stdout().flush().unwrap();
                                    let mut reader = BufReader::new(&stream);
                                    let mut buffer = String::new();
                                    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap(); // Increased timeout
                                    match reader.read_line(&mut buffer) {
                                        Ok(bytes_read) if bytes_read > 0 => {
                                            println!("Received response from {}: {}", peer_addr, buffer.trim());
                                            stdout().flush().unwrap();
                                            if buffer.trim().starts_with("HELLO") {
                                                discovery_peers.lock().unwrap().insert(peer_addr.clone());
                                                println!("Successfully connected to peer: {}", peer_addr);
                                                stdout().flush().unwrap();
                                                let (lock, cvar) = &*discovery_connected;
                                                let mut connected = lock.lock().unwrap();
                                                *connected = true;
                                                cvar.notify_all();
                                            } else {
                                                println!("Peer {} sent invalid response: {}", peer_addr, buffer.trim());
                                                stdout().flush().unwrap();
                                            }
                                        }
                                        _ => {
                                            println!("No response from peer {} within timeout", peer_addr);
                                            stdout().flush().unwrap();
                                        }
                                    }
                                } else {
                                    println!("Failed to send HELLO to {}", peer_addr);
                                    stdout().flush().unwrap();
                                }
                            }
                            Err(e) => {
                                println!("Failed to connect to peer {}: {}", peer_addr, e);
                                stdout().flush().unwrap();
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_secs(5));
            }
            println!("Discovery shut down");
            stdout().flush().unwrap();
        });

        Ok((listener_handle, discovery_handle))
    }

    fn handle_connection(stream: TcpStream, peers: Arc<Mutex<HashSet<String>>>, connected: Arc<(Mutex<bool>, Condvar)>, expected_peer: &str) {
        let peer_addr = stream.peer_addr().unwrap().to_string();
        let peer_ip = peer_addr.split(':').next().unwrap();
        let expected_ip = expected_peer.split(':').next().unwrap();
        {
            let mut peers_guard = peers.lock().unwrap();
            peers_guard.insert(peer_addr.clone());
            if peer_ip == expected_ip {
                println!("Incoming connection from peer IP: {}", peer_addr);
                stdout().flush().unwrap();
                let (lock, cvar) = &*connected;
                let mut connected_guard = lock.lock().unwrap();
                *connected_guard = true;
                cvar.notify_all();
            }
        }
        
        let mut reader = BufReader::new(&stream);
        let mut buffer = String::new();
        
        let message = format!("HELLO from {}", expected_peer);
        if let Ok(mut writer) = stream.try_clone() {
            writer.write_all(message.as_bytes()).unwrap();
            writer.flush().unwrap();
            println!("Sent HELLO to incoming {}", peer_addr);
            stdout().flush().unwrap();
        }

        while let Ok(bytes_read) = reader.read_line(&mut buffer) {
            if bytes_read == 0 {
                break;
            }
            let message = buffer.trim();
            println!("Received from {}: {}", peer_addr, message);
            stdout().flush().unwrap();
            if !message.starts_with("HELLO") {
                println!("\r[{}] {}\n> ", peer_addr, message);
                stdout().flush().unwrap();
            }
            buffer.clear();
        }
        {
            let mut peers_guard = peers.lock().unwrap();
            peers_guard.remove(&peer_addr);
            if peer_ip == expected_ip {
                let (lock, cvar) = &*connected;
                let mut connected_guard = lock.lock().unwrap();
                *connected_guard = false;
                cvar.notify_all();
            }
        }
        println!("Disconnected from {}", peer_addr);
        stdout().flush().unwrap();
    }

    fn send_message(&self, message: &str) {
        println!("Attempting to send message '{}' to {}", message, self.peer_addr);
        stdout().flush().unwrap();
        match TcpStream::connect_timeout(&self.peer_addr.parse().unwrap(), Duration::from_secs(5)) {
            Ok(mut stream) => {
                println!("Connected to {} for sending message", self.peer_addr);
                stdout().flush().unwrap();
                let full_message = format!("{}\n", message);
                match stream.write_all(full_message.as_bytes()) {
                    Ok(_) => {
                        stream.flush().unwrap();
                        println!("Successfully sent message '{}' to {}", message, self.peer_addr);
                        stdout().flush().unwrap();
                    }
                    Err(e) => {
                        println!("Failed to send message '{}' to {}: {}", message, self.peer_addr, e);
                        stdout().flush().unwrap();
                    }
                }
            }
            Err(e) => {
                println!("Failed to connect to {} for message '{}': {}", self.peer_addr, message, e);
                stdout().flush().unwrap();
            }
        }
    }

    fn shutdown(&self) {
        *self.running.lock().unwrap() = false;
    }

    fn wait_for_connection(&self) {
        let (lock, cvar) = &*self.connected;
        let mut connected = lock.lock().unwrap();
        while !*connected {
            println!("Waiting for peer {} to connect...", self.peer_addr);
            stdout().flush().unwrap();
            connected = cvar.wait_timeout(connected, Duration::from_secs(1)).unwrap().0;
            if *connected {
                println!("Peer {} connected!", self.peer_addr);
                stdout().flush().unwrap();
                break;
            }
        }
    }

    fn chat_mode(&self) {
        println!("Entering chat mode with {}", self.peer_addr);
        stdout().flush().unwrap();
        print_prompt();
        loop {
            let mut message = String::new();
            io::stdin().read_line(&mut message).expect("Failed to read message");
            let message = message.trim();
            if message.eq_ignore_ascii_case("exit") {
                println!("Exiting chat mode");
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
            println!("Connected to {}. Choose an option:", self.peer_addr);
            println!("1. Chat");
            println!("2. Disconnect");
            stdout().flush().unwrap();
            print_prompt();

            let mut choice = String::new();
            io::stdin().read_line(&mut choice).expect("Failed to read choice");
            let choice = choice.trim();

            match choice {
                "1" => self.chat_mode(),
                "2" => {
                    println!("Disconnecting...");
                    stdout().flush().unwrap();
                    self.shutdown();
                    break;
                }
                _ => {
                    println!("Invalid choice. Enter 1 or 2.");
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
            let rule_name = "North P2P Network";
            let status = Command::new("netsh")
                .args(&[
                    "advfirewall",
                    "firewall",
                    "add",
                    "rule",
                    &format!("name={}", rule_name),
                    "dir=in",
                    "action=allow",
                    &format!("protocol=TCP"),
                    &format!("localport={}", port),
                ])
                .status();

            match status {
                Ok(status) if status.success() => {
                    println!("Added Windows Firewall rule for port {}", port);
                    stdout().flush().unwrap();
                }
                _ => {
                    println!(
                        "Failed to add Windows Firewall rule. Run as admin: netsh advfirewall firewall add rule name=\"{}\" dir=in action=allow protocol=TCP localport={}",
                        rule_name, port
                    );
                    stdout().flush().unwrap();
                }
            }
        }
        "linux" => {
            if Command::new("ufw").arg("status").output().is_ok() {
                let status = Command::new("ufw")
                    .args(&["allow", &format!("{}/tcp", port)])
                    .status();

                match status {
                    Ok(status) if status.success() => {
                        println!("Added ufw rule for port {}", port);
                        stdout().flush().unwrap();
                    }
                    _ => {
                        println!(
                            "Failed to add ufw rule. Run as root: ufw allow {}/tcp",
                            port
                        );
                        stdout().flush().unwrap();
                    }
                }
            } else if Command::new("iptables").arg("-L").output().is_ok() {
                println!("iptables detected. Manual configuration required: iptables -A INPUT -p tcp --dport {} -j ACCEPT", port);
                stdout().flush().unwrap();
            } else {
                println!("No known firewall detected. Ensure port {} is open manually.", port);
                stdout().flush().unwrap();
            }
        }
        _ => {
            println!("Unsupported OS: {}. Manually open port {} in your firewall.", os, port);
            stdout().flush().unwrap();
        }
    }
}

fn determine_node_config() -> (String, String) {
    const NODE_A_PUBLIC: &str = "47.17.52.8";
    const NODE_B_PUBLIC: &str = "82.25.86.57";
    const PORT: u16 = 8000;

    let local_ip = get_local_ip().unwrap_or_else(|| Ipv4Addr::new(0, 0, 0, 0));
    println!("Detected local IP: {}", local_ip);
    stdout().flush().unwrap();

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
                    println!("Detected server, running as node-b");
                    stdout().flush().unwrap();
                    (
                        format!("{}:{}", NODE_B_PUBLIC, PORT),
                        format!("{}:{}", NODE_A_PUBLIC, PORT),
                    )
                } else {
                    println!("Assuming node-a (Windows)");
                    stdout().flush().unwrap();
                    (
                        format!("{}:{}", NODE_A_PUBLIC, PORT),
                        format!("{}:{}", NODE_B_PUBLIC, PORT),
                    )
                }
            } else {
                println!("Could not determine hostname, defaulting to node-a");
                stdout().flush().unwrap();
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
    println!("Running as {} with peer {}", local_addr, peer_addr);
    stdout().flush().unwrap();

    let network = P2PNetwork::new(&local_addr, &peer_addr);
    let (listener_handle, discovery_handle) = match network.start() {
        Ok(handles) => handles,
        Err(e) => {
            println!("{}", e);
            println!("If UPnP failed, ensure port forwarding is set up manually on your router.");
            stdout().flush().unwrap();
            return;
        }
    };

    network.wait_for_connection();
    network.run_menu();

    network.shutdown();
    listener_handle.join().expect("Listener thread panicked");
    discovery_handle.join().expect("Discovery thread panicked");
    println!("Shutdown complete");
    stdout().flush().unwrap();
}