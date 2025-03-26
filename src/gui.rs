use eframe::egui;
use std::sync::{Arc, Mutex};
use crossbeam::channel::{unbounded, Sender, Receiver};
//khraaaa
pub fn run()->  Result<(), eframe::Error> {
    eframe::run_native(
        "Chat App",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(MyApp::default()))),
    )
}

#[derive(Default)]
pub struct MyApp;

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("Welcome to the Network Simulation!");
        });
    }
}
