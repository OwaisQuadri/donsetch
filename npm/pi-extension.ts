/**
 * DonSeTch pi extension — bridges the donsetch MCP binary into pi.
 *
 * `pi install npm:donsetch` installs this package. At session_start the
 * extension spawns `donsetch mcp`, performs the MCP handshake, discovers
 * tools via tools/list, and registers each one natively with
 * pi.registerTool(). Tool calls are proxied to the binary over stdio.
 *
 * Zero maintenance: tool definitions are fetched dynamically from the
 * binary. When donsetch adds or changes tools, this extension picks
 * them up automatically — no code changes needed here.
 *
 * Auto-download: if the binary is missing (e.g. postinstall was
 * blocked by npm 10+), the extension runs install.js at session_start
 * to fetch it from GitHub Releases.
 */
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import { spawn, execFileSync, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { join, dirname } from "node:path";

// ── Constants ──
const INIT_TIMEOUT_MS = 10_000;
const CALL_TIMEOUT_MS = 120_000; // fetch/crawl can take a while
const SHUTDOWN_GRACE_MS = 2_000;

// ── MCP client state ──
let proc: ChildProcess | null = null;
let nextId = 1;
const pending = new Map<
  number,
  { resolve: (v: any) => void; reject: (e: any) => void; timer: ReturnType<typeof setTimeout> }
>();
let initialized = false;
const toolNames: string[] = [];

// ── Binary resolution ──

function getBinaryPath(): string {
  const pkgDir = __dirname;
  const binaryName = process.platform === "win32" ? "donsetch.exe" : "donsetch";
  return join(pkgDir, "binaries", binaryName);
}

function ensureBinary(): string {
  const binaryPath = getBinaryPath();
  if (existsSync(binaryPath)) return binaryPath;

  // Binary missing — postinstall was likely blocked. Run install.js
  // to download from GitHub Releases.
  const installScript = join(__dirname, "install.js");
  if (!existsSync(installScript)) {
    throw new Error(
      `donsetch binary not found at ${binaryPath} and install.js is missing. ` +
      `Run \`npm rebuild donsetch\` or \`npm install -g --allow-scripts=donsetch donsetch@latest\`.`
    );
  }

  try {
    execFileSync("node", [installScript], {
      stdio: "inherit",
      cwd: __dirname,
      timeout: 60_000,
    });
  } catch (err: any) {
    throw new Error(`Failed to download donsetch binary: ${err.message}`);
  }

  if (!existsSync(binaryPath)) {
    throw new Error(
      `donsetch binary still missing after install.js ran. ` +
      `Run \`npm install -g --allow-scripts=donsetch donsetch@latest\` manually.`
    );
  }

  return binaryPath;
}

// ── MCP JSON-RPC 2.0 over stdio ──

function startServer(): Promise<void> {
  if (proc && initialized) return Promise.resolve();
  if (proc && !initialized) return Promise.reject(new Error("donsetch MCP server is still initializing"));

  return new Promise((resolve, reject) => {
    let binaryPath: string;
    try {
      binaryPath = ensureBinary();
    } catch (err: any) {
      reject(err);
      return;
    }

    try {
      proc = spawn(binaryPath, ["mcp"], {
        stdio: ["pipe", "pipe", "pipe"],
        env: { ...process.env },
        windowsHide: true,
      });
    } catch (err: any) {
      reject(new Error(`Failed to spawn donsetch MCP server: ${err.message}`));
      return;
    }

    let buffer = "";

    proc.stdout?.on("data", (chunk: Buffer) => {
      buffer += chunk.toString();
      const lines = buffer.split("\n");
      buffer = lines.pop() || "";
      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          const msg = JSON.parse(line);
          if (msg.id != null && pending.has(msg.id)) {
            const entry = pending.get(msg.id)!;
            pending.delete(msg.id);
            clearTimeout(entry.timer);
            if (msg.error) {
              entry.reject(new Error(msg.error.message || "MCP error"));
            } else {
              entry.resolve(msg.result);
            }
          }
        } catch {
          /* ignore non-JSON lines on stdout */
        }
      }
    });

    // Drain stderr to prevent pipe buffer deadlock; route to our stderr for debugging.
    proc.stderr?.on("data", (chunk: Buffer) => {
      process.stderr.write(chunk);
    });

    proc.on("error", (err) => {
      proc = null;
      initialized = false;
      for (const [, e] of pending) {
        clearTimeout(e.timer);
        e.reject(err);
      }
      pending.clear();
    });

    proc.on("exit", (code) => {
      proc = null;
      initialized = false;
      for (const [, e] of pending) {
        clearTimeout(e.timer);
        e.reject(new Error(`donsetch MCP server exited (code ${code})`));
      }
      pending.clear();
    });

    // MCP handshake: initialize → notifications/initialized
    sendRequest(
      "initialize",
      {
        protocolVersion: "2024-11-05",
        capabilities: {},
        clientInfo: { name: "pi-donsetch", version: "1.0.0" },
      },
      INIT_TIMEOUT_MS
    )
      .then(() => {
        sendNotification("notifications/initialized", {});
        initialized = true;
        resolve();
      })
      .catch(reject);
  });
}

function sendRequest(method: string, params: any, timeoutMs = CALL_TIMEOUT_MS): Promise<any> {
  return new Promise((resolve, reject) => {
    if (!proc?.stdin?.writable) {
      reject(new Error("donsetch MCP server not running"));
      return;
    }
    const id = nextId++;
    const timer = setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`MCP request timeout (${timeoutMs}ms): ${method}`));
      }
    }, timeoutMs);
    pending.set(id, { resolve, reject, timer });
    const msg = JSON.stringify({ jsonrpc: "2.0", id, method, params });
    proc.stdin.write(msg + "\n");
  });
}

function sendNotification(method: string, params: any): void {
  if (!proc?.stdin?.writable) return;
  const msg = JSON.stringify({ jsonrpc: "2.0", method, params });
  proc.stdin.write(msg + "\n");
}

async function callMcpTool(name: string, args: any): Promise<any> {
  return sendRequest("tools/call", { name, arguments: args ?? {} });
}

function killServer(): void {
  if (proc) {
    try {
      proc.stdin?.end();
      proc.kill("SIGTERM");
      const p = proc;
      setTimeout(() => {
        try {
          p.kill("SIGKILL");
        } catch {}
      }, SHUTDOWN_GRACE_MS);
    } catch {}
    proc = null;
  }
  initialized = false;
  toolNames.length = 0;
  for (const [, e] of pending) {
    clearTimeout(e.timer);
    e.reject(new Error("donsetch MCP server killed"));
  }
  pending.clear();
}

function isAlive(): boolean {
  return proc !== null && !proc.killed && proc.stdin?.writable === true;
}

// ── Extension ──

export default function (pi: ExtensionAPI) {
  pi.on("session_start", async () => {
    try {
      await startServer();
    } catch (err: any) {
      process.stderr.write(`[donsetch] failed to start MCP server: ${err.message}\n`);
      return;
    }

    let toolsResult: any;
    try {
      toolsResult = await sendRequest("tools/list", {});
    } catch (err: any) {
      process.stderr.write(`[donsetch] failed to list tools: ${err.message}\n`);
      return;
    }

    const mcpTools: any[] = toolsResult?.tools ?? [];
    if (mcpTools.length === 0) {
      process.stderr.write("[donsetch] no tools discovered from MCP server\n");
      return;
    }

    for (const mcpTool of mcpTools) {
      const name = mcpTool.name;
      if (!name) continue;
      toolNames.push(name);

      const description = mcpTool.description || mcpTool.name;
      const inputSchema = mcpTool.inputSchema || { type: "object", properties: {} };

      // Capture name for closure
      const toolName = name;

      pi.registerTool({
        name: toolName,
        label: toolName,
        description,
        parameters: Type.Unsafe(inputSchema) as any,
        async execute(_toolCallId, params, signal) {
          // Check if server is still alive, restart if dead
          if (!isAlive()) {
            try {
              await startServer();
            } catch (err: any) {
              return {
                content: [{ type: "text", text: `donsetch MCP server crashed and could not restart: ${err.message}` }],
                isError: true,
              };
            }
          }

          try {
            const result = await callMcpTool(toolName, params);
            return {
              content: result?.content ?? [{ type: "text", text: "No output" }],
              details: { mcpTool: toolName, isError: result?.isError ?? false },
              isError: result?.isError ?? false,
            };
          } catch (err: any) {
            return {
              content: [{ type: "text", text: `donsetch MCP call failed: ${err.message}` }],
              details: { error: err.message, mcpTool: toolName },
              isError: true,
            };
          }
        },
      });
    }

    process.stderr.write(`[donsetch] ${mcpTools.length} tools registered: ${toolNames.join(", ")}\n`);
  });

  pi.on("session_shutdown", () => {
    killServer();
  });
}
