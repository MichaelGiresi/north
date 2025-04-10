use std::net::{TcpListener, TcpStream, SocketAddrV4, IpAddr, Ipv4Addr};
use std::io::{self, Write, BufReader, BufRead};
use std::sync::{Arc, Mutex, Condvar};
use std::thread;
use std::time::Duration;
use std::collections::HashSet;
use std::env;
use igd::{PortMappingProtocol, SearchOptions};
use if_addrs;

struct P2PNetwork {
    peers: Arc<Mutex<HashSet<String>>>,
    potential_peers: Arc<Mutex<HashSet<String>>>,
    local_addr: String,
    running: Arc<Mutex<bool>>,
    connected: Arc<(Mutex<bool>, Condvar)>,
}

impl P2PNetwork {
    fn new(local_addr: &str, initial_peer: &str) -> Self {
        let mut potential_peers = HashSet::new();
        if !initial_peer.is_empty() {
            potential_peers.insert(initial_peer.to_string());
        }
        P2PNetwork {
            peers: Arc::new(Mutex::new(HashSet::new())),
            potential_peers: Arc::new(Mutex::new(potential_peers)),
            local_addr: local_addr.to_string(),
            running: Arc::new(Mutex::new(true)),
            connected: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn start(&self) -> Result<(thread::JoinHandle<()>, thread::JoinHandle<()>), String> {
        let port = self.local_addr.split(':').last().unwrap().parse::<u16>()
            .map_err(|_| "Invalid port number".to_string())?;
        let bind_addr = format!("0.0.0.0:{}", port);

        // Get the local IP address for UPnP
        let local_ip = get_local_ip().unwrap_or_else(|| {
            println!("Failed to determine local IP, falling back to 0.0.0.0");
            Ipv4Addr::new(0, 0, 0, 0)
        });
        println!("Using local IP for UPnP: {}", local_ip);

        // Attempt UPnP port forwarding
        match igd::search_gateway(SearchOptions::default()) {
            Ok(gateway) => {
                let local_addr = SocketAddrV4::new(local_ip, port);
                match gateway.get_external_ip() {
                    Ok(external_ip) => println!("Requesting UPnP mapping: {} -> {}:{}", external_ip, local_ip, port),
                    Err(e) => println!("Failed to get external IP: {}", e),
                }
                match gateway.add_port(PortMappingProtocol::TCP, port, local_addr, 3600, "P2P Network") {
                    Ok(()) => println!("UPnP port forwarding set up for port {}", port),
                    Err(e) => println!("Failed to set up UPnP: {}. Manual port forwarding may be required.", e),
                }
            }
            Err(e) => println!("UPnP gateway not found: {}. Manual port forwarding may be required.", e),
        }

        let listener = TcpListener::bind(&bind_addr)
            .map_err(|e| format!("Failed to bind to {}: {}", bind_addr, e))?;
        println!("Node started at {}", self.local_addr);

        let peers = Arc::clone(&self.peers);
        let running = Arc::clone(&self.running);
        let connected = Arc::clone(&self.connected);

        let listener_handle = thread::spawn(move || {
            for stream in listener.incoming() {
                if !*running.lock().unwrap() {
                    break;
                }
                match stream {
                    Ok(stream) => {
                        let peers_clone = Arc::clone(&peers);
                        let connected_clone = Arc::clone(&connected);
                        thread::spawn(move || {
                            Self::handle_connection(stream, peers_clone, connected_clone);
                        });
                    }
                    Err(e) => println!("Error accepting connection: {}", e),
                }
            }
            println!("Listener shut down");
        });

        let discovery_peers = Arc::clone(&self.peers);
        let discovery_potential = Arc::clone(&self.potential_peers);
        let discovery_running = Arc::clone(&self.running);
        let discovery_connected = Arc::clone(&self.connected);
        let local_addr = self.local_addr.clone();

        let discovery_handle = thread::spawn(move || {
            while *discovery_running.lock().unwrap() {
                let potential = discovery_potential.lock().unwrap().clone();
                for peer_addr in potential {
                    if !discovery_peers.lock().unwrap().contains(&peer_addr) {
                        match TcpStream::connect(&peer_addr) {
                            Ok(mut stream) => {
                                let message = format!("HELLO from {}", local_addr);
                                if stream.write_all(message.as_bytes()).is_ok() {
                                    let mut reader = BufReader::new(&stream);
                                    let mut buffer = String::new();
                                    if reader.read_line(&mut buffer).is_ok() && buffer.trim().starts_with("HELLO") {
                                        discovery_peers.lock().unwrap().insert(peer_addr.clone());
                                        println!("Connected to peer: {}", peer_addr);
                                        let (lock, cvar) = &*discovery_connected;
                                        let mut connected = lock.lock().unwrap();
                                        *connected = true;
                                        cvar.notify_all();
                                    } else {
                                        println!("Peer {} responded but isn't a valid node", peer_addr);
                                    }
                                }
                            }
                            Err(_) => {}
                        }
                    }
                }
                thread::sleep(Duration::from_secs(5));
            }
            println!("Discovery shut down");
        });

        Ok((listener_handle, discovery_handle))
    }

    fn handle_connection(stream: TcpStream, peers: Arc<Mutex<HashSet<String>>>, connected: Arc<(Mutex<bool>, Condvar)>) {
        let peer_addr = stream.peer_addr().unwrap().to_string();
        {
            let mut peers_guard = peers.lock().unwrap();
            peers_guard.insert(peer_addr.clone());
            let (lock, cvar) = &*connected;
            let mut connected_guard = lock.lock().unwrap();
            *connected_guard = true;
            cvar.notify_all();
        }
        
        let mut reader = BufReader::new(&stream);
        let mut buffer = String::new();
        
        let message = format!("HELLO from {}", stream.local_addr().unwrap());
        if let Ok(mut writer) = stream.try_clone() {
            writer.write_all(message.as_bytes()).unwrap();
        }

        while let Ok(bytes_read) = reader.read_line(&mut buffer) {
            if bytes_read == 0 {
                break;
            }
            let message = buffer.trim();
            if !message.starts_with("HELLO") {
                println!("\r[{}] {}\n> ", peer_addr, message);
                print_prompt();
            }
            buffer.clear();
        }
        peers.lock().unwrap().remove(&peer_addr);
        println!("Disconnected from {}", peer_addr);
    }

    fn send_message(&self, message: &str) {
        let peers = self.peers.lock().unwrap().clone();
        for peer_addr in peers {
            if let Ok(mut stream) = TcpStream::connect(&peer_addr) {
                let full_message = format!("{}\n", message);
                if let Err(e) = stream.write_all(full_message.as_bytes()) {
                    println!("Failed to send to {}: {}", peer_addr, e);
                }
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
            println!("Waiting for a peer to connect...");
            connected = cvar.wait(connected).unwrap();
        }
    }
}

fn print_prompt() {
    print!("> ");
    io::stdout().flush().unwrap();
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

const NODE_A: &str = "47.17.52.8:8000";  // Windows machine
const NODE_B: &str = "82.25.86.57:8000"; // Server

fn main() {
    let args: Vec<String> = env::args().collect();
    let (local_addr, peer_addr) = match args.get(1).map(|s| s.as_str()) {
        Some("node-a") => (NODE_A, NODE_B),
        Some("node-b") => (NODE_B, NODE_A),
        _ => {
            println!("Usage: cargo run -- <node-a|node-b>");
            println!("Defaulting to node-a ({}) with peer node-b ({}).", NODE_A, NODE_B);
            (NODE_A, NODE_B)
        }
    };

    println!("Running as {} with peer {}", local_addr, peer_addr);

    let network = P2PNetwork::new(local_addr, peer_addr);
    let (listener_handle, discovery_handle) = match network.start() {
        Ok(handles) => handles,
        Err(e) => {
            println!("{}", e);
            println!("If UPnP failed, ensure port forwarding is set up manually on your router.");
            return;
        }
    };

    network.wait_for_connection();

    let network_clone = Arc::new(network);
    let sender_network = Arc::clone(&network_clone);
    
    let input_handle = thread::spawn(move || {
        println!("Type messages to send (or 'quit' to exit):");
        print_prompt();
        
        loop {
            let mut message = String::new();
            io::stdin().read_line(&mut message).expect("Failed to read message");
            let message = message.trim();
            
            if message.eq_ignore_ascii_case("quit") {
                break;
            }
            
            if !message.is_empty() {
                sender_network.send_message(message);
                print_prompt();
            }
        }
    });

    input_handle.join().expect("Input thread panicked");
    network_clone.shutdown();
    listener_handle.join().expect("Listener thread panicked");
    discovery_handle.join().expect("Discovery thread panicked");
    println!("Shutdown complete");
}