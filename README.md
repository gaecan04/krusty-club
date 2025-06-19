![5785225144627743089](https://github.com/user-attachments/assets/41a0e24e-04ea-4b03-bf43-e268c06e421c)

# 🛰️ Distributed Drone Chat System

Welcome to the official repository of the **Krusty_Club**, developed for the *Advanced Programming* (AP) course – University of Trento, Academic Year 2024-2025.

## 👥 Project Team

- **Gaetano Cannone** – Team Leader and associated developer 
- **Oussema Kasraoui** – Simulation Controller and GUI developer 
- **Leonardo Zappalà** – Server developer 
- **Chiara Farsetti** – Client developer 

> 📧 Group contact (telegram) : https://t.me/+C2llp28S2BpiMzI0

---

## 🧠 Project Overview

This project simulates a distributed communication system involving:
- **Clients** that register to servers and chat / exchange messages - medias with each other. 
- **Drones** that forward messages and may crash probabilistically
- **Servers** that manage registration, media handling, and chat history. (intermediate point in the communication: C->S->C)

The main objective is to implement a dynamic, and efficient peer-to-peer chat system on a simulated network graph.

---

## 📁 Repository Structure
```
.
krusty-club 
    ├── .idea/                  # Project settings (IDE-specific) 
    ├── assets/                 # Images and visual assets for GUI or reports 
    ├── broadcast media/        # Temp or staging folder for media broadcast 
    ├── media/                  # Stored media 
    ├── src/                    # Main source code: clients, servers, drones, network initializer, simulation controller / GUI.
    ├── topologies/             # Network configuration files (.toml)
    ├── .gitignore              # Git ignore rules
    ├── Cargo.lock              # Dependency lock file
    ├── Cargo.toml              # Project metadata and dependencies
```

### Instruction to run the project:
From terminal: 
``` rust
cargo run --features serialize -- topologies/butterfly.toml (eg)
```
You are invited to try the simulation with different topologies!
