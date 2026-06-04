<script lang="ts">
  import "./app.css";
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import Home from "./pages/Home.svelte";
  import Dashboard from "./pages/Dashboard.svelte";
  import Maintenance from "./pages/Maintenance.svelte";
  import WorkflowBoard from "./pages/WorkflowBoard.svelte";
  import PluginManager from "./pages/PluginManager.svelte";
  import Chat from "./pages/Chat.svelte";
  import Providers from "./pages/Providers.svelte";
  import Agents from "./pages/Agents.svelte";
  import ActivityStateWidget from "./pages/ActivityStateWidget.svelte";
  import NotificationOverlay from "./pages/NotificationOverlay.svelte";
  import Settings from "./pages/Settings.svelte";
  import Integration from "./pages/Integration.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let hasProvider = $state(false);
  let hasAgent = $state(false);
  let activeAgentName = $state("");
  let chatPrefill = $state("");
  let chatPrefillSeq = $state(0);
  let sidebarCompact = $state(false);
  let pluginPages = $state<{ id: string; label: string }[]>([]);
  let gatewayPort = $state(9999);
  let secretsMode = $state("env");
  let teamPageVersion = $state(0);
  // NOT $state — postMessage updates must not trigger iframe src reload.
  // The path is read at render time (when teamPageVersion changes).
  let teamIframePath = "/api/v1/team";
  let hasTeamPlugin = $derived(pluginPages.some(p => p.id === "team"));
  let nonTeamPluginPages = $derived(pluginPages.filter(p => p.id !== "team"));

  type MenuItem = { id: string; label: string; short: string };
  type MenuGroup = { name: string; label: string; items: MenuItem[] };

  const menuGroups: MenuGroup[] = [
    {
      name: "apps",
      label: "Workspace",
      items: [
        { id: "home", label: "Home", short: "Ho" },
        { id: "chat", label: "Chat", short: "Ch" },
      ],
    },
    {
      name: "platform",
      label: "Services",
      items: [
        { id: "agents", label: "Agents", short: "Ag" },
        { id: "providers", label: "Providers", short: "Pr" },
        { id: "integration", label: "Integration", short: "In" },
        { id: "dashboard", label: "Dashboard", short: "Db" },
      ],
    },
    {
      name: "management",
      label: "Management",
      items: [
        { id: "workflows", label: "Workflow Board", short: "Wf" },
        { id: "plugins", label: "Plugin Manager", short: "Pl" },
        { id: "maintenance", label: "Maintenance", short: "Ma" },
        { id: "settings", label: "Settings", short: "Se" },
      ],
    },
  ];

  // Hide Integration when secrets are read from env vars (Keychain unused).
  let visibleMenuGroups = $derived(
    menuGroups.map(g => ({
      ...g,
      items: g.items.filter(item => !(item.id === "integration" && secretsMode === "env")),
    })).filter(g => g.items.length > 0)
  );

  let expandedGroups = $state<Record<string, boolean>>({
    apps: true,
    platform: true,
    management: true,
    plugins: true,
  });

  let initialLoadDone = $state(false);
  let shuttingDown = $state(false);
  let shutdownComplete = $state(false);

  function toggleGroup(name: string) {
    expandedGroups[name] = !expandedGroups[name];
  }

  async function refreshPluginPages() {
    try {
      pluginPages = await invoke<{ id: string; label: string }[]>("get_plugin_pages");
    } catch {
      pluginPages = [];
    }
  }

  async function checkOnboarding() {
    try {
      const [hp, ha] = await Promise.all([
        invoke<boolean>("has_any_provider"),
        invoke<boolean>("has_any_agent"),
      ]);
      hasProvider = hp;
      hasAgent = ha;

      if (!hp && (currentPage === "home" || currentPage === "dashboard")) {
        currentPage = "providers";
      }
    } catch {
      // Config may not exist yet
    }
  }

  function onRuntimeStatusChange(running: boolean) {
    runtimeRunning = running;
    if (running) {
      refreshPluginPages();
    } else {
      pluginPages = [];
    }
  }

  function handlePageVisited(pageId: string) {
    if (pageId === "providers" || pageId === "agents") {
      checkOnboarding();
    }
  }

  function navigateTo(pageId: string) {
    currentPage = pageId;
    // Force iframe recreation when navigating to team or plugin pages.
    // WKWebView caches iframe content by URL, so returning to the
    // same src after the iframe was destroyed may show stale/blank
    // content. Incrementing a key forces a fresh DOM element.
    if (pageId === "team" || pageId.startsWith("plugin:")) teamPageVersion++;
    handlePageVisited(pageId);
  }

  function navigateToChatWithPrefill(text: string) {
    chatPrefill = text;
    chatPrefillSeq++;
    navigateTo("chat");
  }

  async function refreshActiveAgent() {
    try {
      const agent = await invoke<{ display_name: string } | null>("get_active_agent");
      activeAgentName = agent?.display_name ?? "";
    } catch {
      activeAgentName = "";
    }
  }

  $effect(() => {
    // Auto-navigate to Dashboard when gateway is not running
    if (!runtimeRunning) {
      currentPage = "dashboard";
      return;
    }
    // Refresh active agent on every page change so the idle widget
    // shows/hides correctly regardless of which page the agent is used from.
    void currentPage;
    refreshActiveAgent();
  });

  onMount(async () => {
    await checkOnboarding();

    // Read secrets mode to decide whether to show Integration.
    try {
      secretsMode = JSON.parse(await invoke<string>("get_secrets_mode"));
    } catch {
      secretsMode = "env";
    }

    // Get gateway port for plugin iframe URLs
    try {
      gatewayPort = await invoke<number>("get_gateway_port");
    } catch {
      // keep default
    }

    // Auto-detect if gateway is already running
    try {
      const status = await invoke<{ running: boolean }>("try_connect_gateway");
      if (status.running) {
        runtimeRunning = true;
        currentPage = "home";
        await refreshPluginPages();
      }
    } catch {
      // Gateway not running — stay on dashboard
    }

    initialLoadDone = true;

    // ── postMessage bridge: handle messages from iframes ─────────────
    // Team and plugin pages are loaded in iframes and cannot invoke Tauri
    // commands directly. This bridge handles:
    //   - aman:confirm    → native OS confirm dialog
    //   - aman:team-url   → save current iframe URL for restore on revisit
    window.addEventListener("message", async (event: MessageEvent) => {
      const data = event.data;
      if (!data) return;

      // ── aman:team-url — remember iframe's current URL ──────────
      if (data.type === "aman:team-url" && typeof data.url === "string") {
        teamIframePath = data.url;
        return;
      }

      // ── aman:confirm — native OS confirm dialog ────────────────
      if (data.type !== "aman:confirm") return;
      const source = event.source as Window | null;
      if (!source) return;

      // Acknowledge immediately so the iframe cancels its fallback timer.
      // The native dialog may stay open for several seconds while the user
      // reads the message and decides — the iframe must not time out.
      source.postMessage({ type: "aman:confirm-ack" }, "*");

      try {
        const confirmed = await invoke<boolean>("show_confirm_dialog", {
          title: data.title || "Confirm",
          message: data.message || "Are you sure?",
          confirmLabel: data.confirmLabel || null,
          cancelLabel: data.cancelLabel || null,
        });
        source.postMessage({ type: "aman:confirm-result", confirmed }, "*");
      } catch {
        source.postMessage({ type: "aman:confirm-result", confirmed: false }, "*");
      }
    });

    listen("shutdown:started", () => {
      shuttingDown = true;
    });
    listen("shutdown:complete", () => {
      shutdownComplete = true;
    });
    listen("agent:selected", () => {
      refreshActiveAgent();
    });
  });
</script>

<nav class="sidebar" class:compact={sidebarCompact}>
  {#if sidebarCompact}
    <!-- Compact mode: flat icon-only items -->
    <div class="runtime-dot-row" class:live={runtimeRunning}>
      <span class="runtime-mini-dot"></span>
    </div>
    {#each visibleMenuGroups as group}
      {#each group.items as page}
        {@const isDisabled = !runtimeRunning || page.id === "settings"}
        {#if isDisabled}
          <span class="nav-icon disabled" title={page.id === "settings" ? "Settings are being reorganised" : page.label + " - Start the runtime first"}>
            <span class="nav-short">{page.short}</span>
          </span>
        {:else}
          <button
            class="nav-icon"
            class:active={currentPage === page.id}
            onclick={() => navigateTo(page.id)}
            title={page.label}
          >
            <span class="nav-short">{page.short}</span>
          </button>
        {/if}
      {/each}
      {#if group.name === "apps" && hasTeamPlugin}
        <button
          class="nav-icon"
          class:active={currentPage === "team"}
          onclick={() => navigateTo("team")}
          title="Team"
        >
          <span class="nav-short">Te</span>
        </button>
      {/if}
    {/each}
    {#each nonTeamPluginPages as pg}
      <button
        class="nav-icon"
        class:active={currentPage === "plugin:" + pg.id}
        onclick={() => navigateTo("plugin:" + pg.id)}
        title={pg.label}
      >
        <span class="nav-short">{pg.label.slice(0, 2)}</span>
      </button>
    {/each}
  {:else}
    <!-- Expanded mode: grouped with headers -->
    <div class="sidebar-status-bar" class:live={runtimeRunning}>
      <span class="runtime-status-dot"></span>
      <span class="runtime-status-label">{runtimeRunning ? "Runtime Online" : "Runtime Offline"}</span>
    </div>
    {#each visibleMenuGroups as group}
      <button class="menu-header" onclick={() => toggleGroup(group.name)}>
        <span class="menu-arrow">{expandedGroups[group.name] ? "▾" : "▸"}</span>
        {group.label}
      </button>
      {#if expandedGroups[group.name]}
        <div class="menu-items">
          {#each group.items as page}
            {@const isDisabled = !runtimeRunning || page.id === "settings"}
            {#if isDisabled}
              <span class="disabled" title={page.id === "settings" ? "Settings are being reorganised" : "Start the runtime first"}>
                {page.label}
              </span>
            {:else}
              <button
                class="nav-btn"
                class:active={currentPage === page.id}
                onclick={() => navigateTo(page.id)}
              >
                {page.label}
              </button>
            {/if}
          {/each}
          {#if group.name === "apps" && hasTeamPlugin}
            <button
              class="nav-btn"
              class:active={currentPage === "team"}
              onclick={() => navigateTo("team")}
            >
              Team
            </button>
          {/if}
        </div>
      {/if}
    {/each}
    {#if nonTeamPluginPages.length > 0}
      <button class="menu-header" onclick={() => toggleGroup("plugins")}>
        <span class="menu-arrow">{expandedGroups["plugins"] ? "▾" : "▸"}</span>
        Plugins
      </button>
      {#if expandedGroups["plugins"]}
        <div class="menu-items">
          {#each nonTeamPluginPages as pg}
            <button
              class="nav-btn"
              class:active={currentPage === "plugin:" + pg.id}
              onclick={() => navigateTo("plugin:" + pg.id)}
            >
              {pg.label}
            </button>
          {/each}
        </div>
      {/if}
    {/if}
  {/if}

  <ActivityStateWidget {runtimeRunning} visible={activeAgentName !== ""} agentName={activeAgentName} compact={sidebarCompact} />
</nav>

<NotificationOverlay onNavigate={(p) => navigateTo(p)} />

<main class="main">
  {#if currentPage === "home"}
    <Home
      onNavigate={(p) => navigateTo(p)}
      onNavigateChatWithSkill={async (_agentKey: string, skillName: string) => {
        navigateToChatWithPrefill(`/skill ${skillName} `);
      }}
    />
  {:else if currentPage === "dashboard"}
    <Dashboard onstatuschange={(r) => onRuntimeStatusChange(r)} />
  {:else if currentPage === "maintenance"}
    <Maintenance />
  {:else if currentPage === "workflows"}
    <WorkflowBoard />
  {:else if currentPage === "plugins"}
    <PluginManager />
  {:else if currentPage === "providers"}
    <Providers />
  {:else if currentPage === "integration"}
    <Integration />
  {:else if currentPage === "agents"}
    <Agents onNavigate={(p) => navigateTo(p)} />
  {:else if currentPage === "chat"}
    <Chat prefillInput={chatPrefill} prefillSeq={chatPrefillSeq} />
  {:else if currentPage === "team"}
    {#key teamPageVersion}
      <iframe
        class="plugin-iframe"
        src={"http://127.0.0.1:" + gatewayPort + teamIframePath + "?_=" + teamPageVersion}
        title="Team"
        sandbox="allow-scripts allow-same-origin allow-forms allow-modals"
      ></iframe>
    {/key}
  {:else if currentPage === "settings"}
    <Settings />
  {:else if currentPage.startsWith("plugin:")}
    {#key teamPageVersion}
      <iframe
        class="plugin-iframe"
        src={"http://127.0.0.1:" + gatewayPort + "/api/v1/" + currentPage.slice("plugin:".length)}
        title={"Plugin: " + currentPage.slice("plugin:".length)}
        sandbox="allow-scripts allow-same-origin allow-forms allow-modals"
      ></iframe>
    {/key}
  {/if}
</main>

{#if shuttingDown}
  <div class="shutdown-overlay">
    <div class="shutdown-card">
      {#if shutdownComplete}
        <div class="shutdown-check">&#10003;</div>
        <p class="shutdown-text">Gateway stopped</p>
        <p class="shutdown-sub">Closing window...</p>
      {:else}
        <div class="shutdown-spinner"></div>
        <p class="shutdown-text">Shutting down gateway...</p>
        <p class="shutdown-sub">The window will close automatically</p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .plugin-iframe {
    width: 100%;
    height: 100%;
    border: none;
    background: var(--bg);
  }

  .shutdown-overlay {
    position: fixed;
    inset: 0;
    z-index: 9999;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(11, 13, 19, 0.85);
    backdrop-filter: blur(8px);
    -webkit-backdrop-filter: blur(8px);
    animation: fadeIn 0.2s ease;
  }

  .shutdown-card {
    text-align: center;
    padding: 40px 48px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: var(--radius-xl);
    box-shadow: var(--shadow-xl);
    animation: scaleIn 0.3s ease;
  }

  .shutdown-spinner {
    width: 32px;
    height: 32px;
    margin: 0 auto 20px;
    border: 3px solid var(--border-strong);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  .shutdown-check {
    width: 32px;
    height: 32px;
    margin: 0 auto 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 18px;
    color: var(--green);
    border: 2px solid var(--green);
    border-radius: 50%;
    animation: scaleIn 0.3s ease;
  }

  .shutdown-text {
    font-size: 15px;
    font-weight: 600;
    color: var(--fg);
    margin-bottom: 6px;
  }

  .shutdown-sub {
    font-size: 12px;
    color: var(--fg-dim);
  }

  @keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
  }

  @keyframes scaleIn {
    from { opacity: 0; transform: scale(0.95); }
    to { opacity: 1; transform: scale(1); }
  }

  @keyframes spin {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
</style>
