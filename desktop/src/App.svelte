<script lang="ts">
  import "./app.css";
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { cubicOut, cubicIn } from "svelte/easing";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import * as i18n from "./lib/i18n.svelte";
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
  import McpServers from "./pages/McpServers.svelte";
  import AuroraBackground from "./components/AuroraBackground.svelte";
  import ParticleField from "./components/ParticleField.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);
  let hasProvider = $state(false);
  let hasAgent = $state(false);
  let activeAgentName = $state("");
  let activeAgentKey = $state("");
  let chatPrefill = $state("");
  let chatPrefillSeq = $state(0);
  let sidebarCompact = $state(false);
  let pluginPages = $state<{ id: string; label: string }[]>([]);
  let gatewayPort = $state(9999);
  let secretsMode = $state("env");
  let mcpEnabled = $state(false);
  let uiStyle = $state<string>("frosted-glass");
  let teamPageVersion = $state(0);
  // NOT $state — postMessage updates must not trigger iframe src reload.
  // The path is read at render time (when teamPageVersion changes).
  let teamIframePath = "/api/v1/team";

  // Page transition direction: 1 = forward (slide from right), -1 = back (slide from left)
  let navDirection = $state(1);
  let navHistory = $state<string[]>([]);
  let prefersReducedMotion = $state(false);
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
        { id: "mcp-servers", label: "MCP Servers", short: "MC" },
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

  // Hide Providers and Integration when secrets are read from env vars
  // (Keychain unused — providers/models are configured in config files).
  let visibleMenuGroups = $derived(
    menuGroups.map(g => ({
      ...g,
      items: g.items.filter(item => !(
        (item.id === "integration" || item.id === "providers") && secretsMode === "env" ||
        (item.id === "mcp-servers") && !mcpEnabled
      )),
    })).filter(g => g.items.length > 0)
  );

  // Sidebar groups with dynamic Plugins section inserted between
  // Services (platform) and Management.
  let sidebarGroups = $derived(
    visibleMenuGroups
      .filter(g => g.name !== "management")
      .concat(
        (hasTeamPlugin || nonTeamPluginPages.length > 0)
          ? [{
              name: "plugins",
              label: "Plugins",
              items: [
                ...(hasTeamPlugin ? [{ id: "team", label: "Team", short: "Te" }] : []),
                ...nonTeamPluginPages.map(p => ({ id: "plugin:" + p.id, label: p.label, short: p.label.slice(0, 2) })),
              ],
            }]
          : [],
      )
      .concat(visibleMenuGroups.filter(g => g.name === "management")),
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
        navigateTo("providers");
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

  function startWindowDrag() {
    getCurrentWindow().startDragging();
  }

  function navigateTo(pageId: string) {
    if (pageId === currentPage) return;

    // Detect back navigation: if the target page exists in history,
    // it's a back-navigation. Trim history to that point.
    const historyIndex = navHistory.indexOf(pageId);
    if (historyIndex >= 0) {
      navDirection = -1;
      navHistory = navHistory.slice(0, historyIndex);
    } else {
      navDirection = 1;
      navHistory = [...navHistory, currentPage];
    }

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
      const agent = await invoke<{ key: string; display_name: string } | null>("get_active_agent");
      activeAgentName = agent?.display_name ?? "";
      activeAgentKey = agent?.key ?? "";
    } catch {
      activeAgentName = "";
      activeAgentKey = "";
    }
  }

  $effect(() => {
    // Auto-navigate to Dashboard when gateway is not running
    if (!runtimeRunning) {
      navigateTo("dashboard");
      return;
    }
    // Refresh active agent on every page change so the idle widget
    // shows/hides correctly regardless of which page the agent is used from.
    void currentPage;
    refreshActiveAgent();
  });

  onMount(async () => {
    // Respect OS reduced-motion preference for page transitions
    prefersReducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    await checkOnboarding();

    // Load locale from backend config (ui.locale).
    try {
      const loc = await invoke<{ code: string; display: string }>("get_locale");
      i18n.setLocale({ code: loc.code as i18n.LocaleCode, display: loc.display });
    } catch {
      // keep default (en)
    }

    // Load UI style from backend config (ui.style).
    try {
      uiStyle = await invoke<string>("get_ui_style");
    } catch {
      // keep default (frosted-glass)
    }

    // Read secrets mode to decide whether to show Integration.
    try {
      secretsMode = JSON.parse(await invoke<string>("get_secrets_mode"));
    } catch {
      secretsMode = "env";
    }

    // Read MCP enabled status to decide whether to show MCP Servers page.
    try {
      mcpEnabled = await invoke<boolean>("get_mcp_enabled");
    } catch {
      mcpEnabled = false;
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
        navigateTo("home");
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

{#if uiStyle === "aurora"}
  <AuroraBackground />
  <ParticleField />
{/if}

<!-- Transparent drag strip at the very top of the window.
     With titleBarStyle: Overlay, WebView content fills the entire
     window including the title bar area. This strip catches
     mousedown events in the title bar zone and initiates a native
     window drag via startDragging(). Traffic-light buttons are
     rendered by the OS above the WebView, so they still work. -->
<div class="titlebar-drag-strip" onmousedown={startWindowDrag}></div>

<nav class="sidebar" class:compact={sidebarCompact} onmousedown={startWindowDrag}>
  {#if sidebarCompact}
    <!-- Compact mode: flat icon-only items -->
    {#each sidebarGroups as group}
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
    {/each}
  {:else}
    <!-- Expanded mode: grouped with headers -->
    {#each sidebarGroups as group}
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
        </div>
      {/if}
    {/each}
  {/if}

  <ActivityStateWidget {runtimeRunning} visible={activeAgentName !== ""} agentId={activeAgentKey} agentName={activeAgentName} compact={sidebarCompact} />
</nav>

<NotificationOverlay onNavigate={(p) => navigateTo(p)} />

<main class="main">
  {#key currentPage}
    <div
      class="page-wrapper"
      in:fly={prefersReducedMotion ? { x: 0, duration: 0 } : { x: 80 * navDirection, duration: 250, easing: cubicOut }}
      out:fly={prefersReducedMotion ? { x: 0, duration: 0 } : { x: -80 * navDirection, duration: 200, easing: cubicIn }}
    >
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
          ></iframe>
        {/key}
      {:else if currentPage === "settings"}
        <Settings />
      {:else if currentPage === "mcp-servers"}
        <McpServers />
      {:else if currentPage.startsWith("plugin:")}
        {#key teamPageVersion}
          <iframe
            class="plugin-iframe"
            src={"http://127.0.0.1:" + gatewayPort + "/api/v1/" + currentPage.slice("plugin:".length)}
            title={"Plugin: " + currentPage.slice("plugin:".length)}
          ></iframe>
        {/key}
      {/if}
    </div>
  {/key}
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
    backdrop-filter: blur(var(--glass-blur-far));
    -webkit-backdrop-filter: blur(var(--glass-blur-far));
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
