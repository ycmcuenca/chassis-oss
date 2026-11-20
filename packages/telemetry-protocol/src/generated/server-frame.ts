// Generated from JSON Schema. Run pnpm run generate to regenerate.

export type ServerFrame = {
  /**
   * Per-connection frame sequence number, starting at one.
   */
  seq: number;
  /**
   * Sender wall-clock time in Unix epoch milliseconds.
   */
  ts_ms: number;
  /**
   * Protocol major version number.
   */
  v: number;
} & (
  | {
      capabilities: Capabilities;
      daemon_version: string;
      host: HostIdentity;
      protocol: ProtocolVersion;
      /**
       * Sampling interval in milliseconds.
       */
      sample_interval_ms: number;
      type: "hello";
    }
  | {
      cpu: CpuSample;
      disk: DiskSample;
      gpus: GpuSample[];
      memory: MemorySample;
      network: NetworkSample;
      top_processes: ProcessSample[];
      type: "host.sample";
      /**
       * Host uptime in seconds.
       */
      uptime_s: number;
    }
  | {
      sessions: AgentSession[];
      type: "agents.snapshot";
    }
  | {
      session: AgentSession;
      type: "agent.upsert";
    }
  | {
      id: string;
      type: "agent.remove";
    }
  | {
      code: ErrorCode;
      message: string;
      type: "error";
    }
);
export type AvailabilityStatus = "available" | "partial" | "unavailable";
export type AgentKind = "claude_code" | "opencode" | "codex";
export type Cost =
  | {
      price_table?: string | null;
      source: "reported";
      /**
       * Cost in US dollars.
       */
      usd: number;
    }
  | {
      price_table: string;
      source: "estimated";
      /**
       * Cost in US dollars.
       */
      usd: number;
    };
export type AgentState = "thinking" | "tool_use" | "working" | "awaiting_approval" | "idle" | "unknown";
export type StateSource = "reported" | "inferred";
export type ErrorCode = "auth_failed" | "protocol_mismatch" | "internal";

export interface Capabilities {
  agents: AgentCapability[];
  metrics: MetricCapabilities;
}
export interface AgentCapability {
  availability: AvailabilityStatus;
  kind: AgentKind;
  reason?: string | null;
}
export interface MetricCapabilities {
  gpu_freq: Availability;
  gpu_memory: Availability;
  gpu_power: Availability;
  gpu_usage: Availability;
  temperatures: Availability;
}
export interface Availability {
  reason?: string | null;
  status: AvailabilityStatus;
}
export interface HostIdentity {
  arch: string;
  cpu_brand: string;
  /**
   * Number of CPU cores.
   */
  cpu_cores: number;
  hostname: string;
  id: string;
  /**
   * Total host memory in bytes.
   */
  memory_total_bytes: number;
  os_name: string;
  os_version: string;
}
export interface ProtocolVersion {
  /**
   * Protocol major version number.
   */
  major: number;
  /**
   * Protocol minor version number.
   */
  minor: number;
}
export interface CpuSample {
  /**
   * Usage of each CPU core as a percentage (0–100).
   */
  per_core_pct: number[];
  /**
   * Overall CPU usage as a percentage (0–100).
   */
  usage_pct: number;
}
export interface DiskSample {
  /**
   * Disk read throughput in bytes per second.
   */
  read_bytes_per_s: number;
  /**
   * Disk write throughput in bytes per second.
   */
  write_bytes_per_s: number;
}
export interface GpuSample {
  /**
   * GPU frequency in megahertz.
   */
  freq_mhz?: number | null;
  name: string;
  /**
   * GPU power in watts.
   */
  power_w?: number | null;
  /**
   * GPU temperature in degrees Celsius.
   */
  temp_c?: number | null;
  /**
   * GPU usage as a percentage (0–100).
   */
  usage_pct?: number | null;
  vendor?: string | null;
  /**
   * Total video memory in bytes.
   */
  vram_total_bytes?: number | null;
  /**
   * Used video memory in bytes.
   */
  vram_used_bytes?: number | null;
}
export interface MemorySample {
  /**
   * Available memory in bytes.
   */
  available_bytes: number;
  /**
   * Total swap in bytes.
   */
  swap_total_bytes: number;
  /**
   * Used swap in bytes.
   */
  swap_used_bytes: number;
  /**
   * Total memory in bytes.
   */
  total_bytes: number;
  /**
   * Used memory in bytes.
   */
  used_bytes: number;
}
export interface NetworkSample {
  interfaces: NetworkInterfaceSample[];
  /**
   * Aggregate receive throughput in bytes per second.
   */
  rx_bytes_per_s: number;
  /**
   * Aggregate transmit throughput in bytes per second.
   */
  tx_bytes_per_s: number;
}
export interface NetworkInterfaceSample {
  name: string;
  /**
   * Interface receive throughput in bytes per second.
   */
  rx_bytes_per_s: number;
  /**
   * Interface transmit throughput in bytes per second.
   */
  tx_bytes_per_s: number;
}
export interface ProcessSample {
  /**
   * Process CPU usage as a percentage (0–100).
   */
  cpu_pct: number;
  /**
   * Process memory usage in bytes.
   */
  memory_bytes: number;
  name: string;
  /**
   * OS process identifier.
   */
  pid: number;
}
export interface AgentSession {
  context?: ContextUsage | null;
  cost?: Cost | null;
  id: string;
  kind: AgentKind;
  /**
   * Last activity in Unix epoch milliseconds.
   */
  last_activity_ms: number;
  model?: string | null;
  native_id: string;
  /**
   * Output rate in tokens per second.
   */
  output_tokens_per_s: number;
  parent_id?: string | null;
  /**
   * OS process identifier, when known.
   */
  pid?: number | null;
  rate_limits: RateLimit[];
  state: AgentState;
  /**
   * Start of the current state in Unix epoch milliseconds.
   */
  state_since_ms: number;
  state_source: StateSource;
  title?: string | null;
  tokens: TokenCounts;
  workspace_dir: string;
}
export interface ContextUsage {
  /**
   * Context usage in tokens.
   */
  used_tokens?: number | null;
  /**
   * Context window capacity in tokens.
   */
  window_tokens?: number | null;
}
export interface RateLimit {
  /**
   * Scheduled reset in Unix epoch milliseconds.
   */
  resets_at_ms?: number | null;
  /**
   * Used share of the rate limit as a percentage (0–100).
   */
  used_pct: number;
  /**
   * Length of the rate-limit window in minutes.
   */
  window_minutes: number;
}
export interface TokenCounts {
  /**
   * Cumulative cache-read token count.
   */
  cache_read_tokens: number;
  /**
   * Cumulative cache-write token count.
   */
  cache_write_tokens: number;
  /**
   * Cumulative input token count.
   */
  input_tokens: number;
  /**
   * Cumulative output token count.
   */
  output_tokens: number;
  /**
   * Cumulative reasoning token count, when known.
   */
  reasoning_tokens?: number | null;
}
