<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  interface McpServerEntry {
    name: string;
    transport: string;
    command: string | null;
    args: string[];
    url: string | null;
    env: Record<string, string>;
    headers: Record<string, string>;
    auto_connect: boolean;
    source: string;
    connected: boolean;
    tool_count: number;
    error: string | null;
  }

  let servers = $state<McpServerEntry[]>([]);
  let agentKeys = $state<string[]>([]);
  let loading = $state(true);
  let error = $state("");

  // Create form
  let showCreateForm = $state(false);
  let newName = $state("");
  let newTransport = $state("auto");
  let newCommand = $state("");
  let newArgsStr = $state("");
  let newUrl = $state("");
  let newAutoConnect = $state(true);
  let newAgentKey = $state(""); // "" = global

  async function loadServers() {
    loading = true;
    error = "";
    try {
      servers = await invoke<McpServerEntry[]>("list_mcp_servers");
      agentKeys = await invoke<string[]>("list_agent_keys");
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function createServer() {
    if (!newName.trim()) return;
    // Auto-detect: either command or url must be provided
    if ((newTransport === "auto" || newTransport === "stdio") && !newCommand.trim()
        && (newTransport === "auto" || newTransport === "streamable-http") && !newUrl.trim()) return;
    if (newTransport === "stdio" && !newCommand.trim()) return;
    if (newTransport === "streamable-http" && !newUrl.trim()) return;

    const args = newArgsStr
      .split(",")
      .map(s => s.trim())
      .filter(s => s.length > 0);

    // For auto mode, pass the command/url as-is (null if empty)
    const cmd = newCommand.trim() || null;
    const targetUrl = newUrl.trim() || null;

    try {
      await invoke("create_mcp_server", {
        name: newName.trim(),
        transport: newTransport,
        command: cmd,
        args,
        url: targetUrl,
        env: {},
        headers: {},
        autoConnect: newAutoConnect,
        agentKey: newAgentKey || null,
      });
      newName = "";
      newTransport = "auto";
      newCommand = "";
      newArgsStr = "";
      newUrl = "";
      newAutoConnect = true;
      newAgentKey = "";
      showCreateForm = false;
      await loadServers();
    } catch (e) {
      error = String(e);
    }
  }

  async function deleteServer(name: string, source: string) {
    if (!confirm(`删除 MCP server "${name}"?`)) return;
    const agentKey = source === "global" ? null : source;
    try {
      await invoke("delete_mcp_server", { name, agentKey });
      await loadServers();
    } catch (e) {
      error = String(e);
    }
  }

  async function connectServer(agentKey: string, name: string) {
    try {
      await invoke("connect_mcp_server", { agentKey, name });
      await loadServers();
    } catch (e) {
      error = String(e);
    }
  }

  async function disconnectServer(agentKey: string, name: string) {
    try {
      await invoke("disconnect_mcp_server", { agentKey, name });
      await loadServers();
    } catch (e) {
      error = String(e);
    }
  }

  function sourceLabel(source: string): string {
    return source === "global" ? "Global" : `Agent: ${source}`;
  }

  function sourceClass(source: string): string {
    return source === "global" ? "badge-dim" : "badge";
  }

  onMount(() => { loadServers(); });
</script>

<div class="page-header">
  <h2>MCP Servers</h2>
  <button onclick={() => { showCreateForm = !showCreateForm; }}>
    {showCreateForm ? "取消" : "+ 新增"}
  </button>
</div>

{#if error}
  <div class="toast toast-error">{error}</div>
{/if}

<!-- Create Form -->
{#if showCreateForm}
  <div class="card form-card">
    <h3>新增 MCP Server</h3>

    <div class="form-field">
      <label for="new-name">Name *</label>
      <input id="new-name" type="text" placeholder="例如: filesystem" bind:value={newName} />
    </div>

    <div class="form-field">
      <label for="new-transport">Transport</label>
      <select id="new-transport" bind:value={newTransport}>
        <option value="auto">auto (自动检测)</option>
        <option value="stdio">stdio (本地子进程)</option>
        <option value="streamable-http">streamable-http (远程 HTTP)</option>
      </select>
      <span class="form-hint dim">auto: 填写 Command 使用本地子进程，填写 URL 使用远程 HTTP</span>
    </div>

    <div class="form-field">
      <label for="new-command">Command{#if newTransport === 'stdio'} *{/if}</label>
      <input id="new-command" type="text" placeholder="例如: npx" bind:value={newCommand} />
    </div>

    {#if newTransport === "auto" || newTransport === "stdio"}
      <div class="form-field">
        <label for="new-args">Args (逗号分隔)</label>
        <input id="new-args" type="text" placeholder="例如: -y,@modelcontextprotocol/server-filesystem,/path" bind:value={newArgsStr} />
      </div>
    {/if}

    <div class="form-field">
      <label for="new-url">URL{#if newTransport === 'streamable-http'} *{/if}</label>
      <input id="new-url" type="text" placeholder="例如: http://localhost:8000/mcp" bind:value={newUrl} />
    </div>

    <div class="form-field">
      <label for="new-agent">分配至</label>
      <select id="new-agent" bind:value={newAgentKey}>
        <option value="">Global (所有 Agent 共享)</option>
        {#each agentKeys as key}
          <option value={key}>Agent: {key}</option>
        {/each}
      </select>
    </div>

    <div class="form-field">
      <label class="checkbox-label">
        <input type="checkbox" bind:checked={newAutoConnect} />
        启动时自动连接
      </label>
    </div>

    <div class="form-actions">
      <button class="secondary" onclick={() => { showCreateForm = false; }}>取消</button>
      <button onclick={createServer}
        disabled={!newName.trim() || (newTransport === "stdio" && !newCommand.trim()) || (newTransport === "streamable-http" && !newUrl.trim()) || (newTransport === "auto" && !newCommand.trim() && !newUrl.trim())}>
        创建
      </button>
    </div>
  </div>
{/if}

<!-- List -->
{#if loading}
  <p class="dim">加载中...</p>
{:else if servers.length === 0}
  <div class="card empty-state">
    <p>还没有配置任何 MCP Server。</p>
    <p class="dim">点击"+ 新增"按钮添加 MCP Server，让 Agent 调用外部工具。</p>
    <p class="dim">例如：连接 filesystem server 让 Agent 读写文件，或连接 postgres server 查询数据库。</p>
  </div>
{:else}
  <div class="server-list">
    {#each servers as server}
      <div class="card server-card">
        <div class="server-header">
          <strong class="server-name">{server.name}</strong>
          <span class="badge {server.transport === 'stdio' ? '' : 'badge-dim'}">{server.transport}</span>
          <span class="badge {sourceClass(server.source)}">{sourceLabel(server.source)}</span>
          {#if server.auto_connect}
            <span class="badge badge-dim">auto</span>
          {/if}
        </div>

        <div class="server-detail">
          {#if server.command}
            <span class="dim">Command:</span> {server.command}
            {#if server.args.length > 0}
              <span class="dim"> ({server.args.join(", ")})</span>
            {/if}
          {/if}
          {#if server.url}
            {#if server.command} | {/if}
            <span class="dim">URL:</span> {server.url}
          {/if}
        </div>

        <div class="server-detail">
          <span class="dim">Status:</span>
          {#if server.connected}
            <span class="badge ok">Connected ({server.tool_count} tools)</span>
          {:else if server.error}
            <span class="badge warn">Error</span>
            <span class="error-text">{server.error}</span>
          {:else}
            <span class="badge warn">Disconnected</span>
          {/if}
        </div>

        <div class="server-actions">
          {#if server.connected}
            <button class="secondary" onclick={() => disconnectServer(server.source, server.name)}>Disconnect</button>
          {:else}
            <button onclick={() => connectServer(server.source, server.name)}>Connect</button>
          {/if}
          <button class="danger" onclick={() => deleteServer(server.name, server.source)}>删除</button>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 20px;
  }
  .page-header h2 { margin: 0; font-size: 18px; }
  .form-card { max-width: 520px; }
  .form-field {
    margin-bottom: 12px;
  }
  .form-field label {
    display: block;
    font-size: 12px;
    color: var(--fg-dim);
    margin-bottom: 4px;
  }
  .form-field input, .form-field select {
    width: 100%;
    box-sizing: border-box;
  }
  .checkbox-label {
    display: flex !important;
    align-items: center;
    gap: 8px;
    cursor: pointer;
  }
  .checkbox-label input { width: auto; }
  .form-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    margin-top: 16px;
  }
  .server-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
    gap: 16px;
  }
  .server-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    flex-wrap: wrap;
  }
  .server-name { font-size: 15px; }
  .server-detail {
    font-size: 13px;
    margin-bottom: 4px;
  }
  .server-actions {
    display: flex;
    gap: 8px;
    margin-top: 12px;
  }
  .error-text {
    font-size: 12px;
    color: var(--red);
    margin-left: 6px;
  }
  .dim { color: var(--fg-dim); }
  .empty-state { text-align: center; padding: 40px; }
  .toast-error {
    background: var(--red-muted);
    color: var(--red);
    padding: 10px 16px;
    border-radius: 6px;
    margin-bottom: 16px;
    font-size: 13px;
  }
  .badge-dim {
    background: var(--bg-muted);
    color: var(--fg-dim);
  }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 500;
  }
  .badge.ok {
    background: var(--green-muted);
    color: var(--green);
  }
  .badge.warn {
    background: var(--orange-muted);
    color: var(--orange);
  }
</style>
