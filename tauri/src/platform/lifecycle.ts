import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { PlatformLifecycle, ServerLogEntry } from '@/platform/types';

class TauriLifecycle implements PlatformLifecycle {
  onServerReady?: () => void;

  async startServer(remote = false, modelsDir?: string | null): Promise<string> {
    try {
      const result = await invoke<string>('start_server', {
        remote,
        modelsDir: modelsDir ?? undefined,
      });
      console.log('Server started:', result);
      this.onServerReady?.();
      return result;
    } catch (error) {
      console.error('Failed to start server:', error);
      throw error;
    }
  }

  async stopServer(): Promise<void> {
    try {
      await invoke('stop_server');
      console.log('Server stopped');
    } catch (error) {
      console.error('Failed to stop server:', error);
      throw error;
    }
  }

  async restartServer(modelsDir?: string | null): Promise<string> {
    try {
      const result = await invoke<string>('restart_server', {
        modelsDir: modelsDir ?? undefined,
      });
      console.log('Server restarted:', result);
      this.onServerReady?.();
      return result;
    } catch (error) {
      console.error('Failed to restart server:', error);
      throw error;
    }
  }

  async setKeepServerRunning(keepRunning: boolean): Promise<void> {
    try {
      await invoke('set_keep_server_running', { keepRunning });
    } catch (error) {
      console.error('Failed to set keep server running setting:', error);
    }
  }

  async getCloseToTray(): Promise<boolean> {
    try {
      return await invoke<boolean>('get_close_to_tray');
    } catch (error) {
      console.error('Failed to read close-to-tray setting:', error);
      // Matches the Rust-side default, so a failed read never makes the
      // toggle claim the window quits when it actually hides.
      return true;
    }
  }

  async setCloseToTray(enabled: boolean): Promise<void> {
    // Deliberately rethrows, unlike setKeepServerRunning: the caller reverts
    // its toggle when the preference didn't actually persist.
    try {
      await invoke('set_close_to_tray', { enabled });
    } catch (error) {
      console.error('Failed to set close-to-tray setting:', error);
      throw error;
    }
  }

  async setBackendOverride(backend?: string | null): Promise<void> {
    try {
      await invoke('set_backend_override', { backend: backend ?? undefined });
    } catch (error) {
      console.error('Failed to set backend override:', error);
      throw error;
    }
  }

  subscribeToServerLogs(callback: (entry: ServerLogEntry) => void): () => void {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void listen<ServerLogEntry>('server-log', (event) => {
      callback(event.payload);
    })
      .then((fn) => {
        if (disposed) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch((error) => {
        console.error('Failed to subscribe to server logs:', error);
      });

    return () => {
      disposed = true;
      unlisten?.();
      unlisten = null;
    };
  }
}

export const tauriLifecycle = new TauriLifecycle();
