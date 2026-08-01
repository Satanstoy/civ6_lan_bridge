import { invoke } from "@tauri-apps/api/core";
import { mountBridgeApp } from "@civ6-lan-bridge/ui";

mountBridgeApp({ platform: "windows", invoke });
