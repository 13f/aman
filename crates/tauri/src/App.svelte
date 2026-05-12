<script lang="ts">
  import "./app.css";
  import Dashboard from "./pages/Dashboard.svelte";
  import SkillEditor from "./pages/SkillEditor.svelte";
  import EventViewer from "./pages/EventViewer.svelte";
  import WorkflowBoard from "./pages/WorkflowBoard.svelte";
  import SoulEditor from "./pages/SoulEditor.svelte";
  import PluginManager from "./pages/PluginManager.svelte";
  import DLQ from "./pages/DLQ.svelte";

  let currentPage = $state("dashboard");
  let runtimeRunning = $state(false);

  type Page = { id: string; label: string };

  const pages: Page[] = [
    { id: "dashboard", label: "Dashboard" },
    { id: "skills", label: "Skill Editor" },
    { id: "events", label: "Event Viewer" },
    { id: "workflows", label: "Workflow Board" },
    { id: "soul", label: "SOUL Editor" },
    { id: "plugins", label: "Plugin Manager" },
    { id: "dlq", label: "DLQ" },
  ];
</script>

<nav class="sidebar">
  <h1>Aman</h1>
  {#each pages as page}
    <a
      href="#"
      class={currentPage === page.id ? "active" : ""}
      onclick={(e) => { e.preventDefault(); currentPage = page.id; }}
    >
      <span class="status-dot {runtimeRunning ? 'running' : 'stopped'}"></span>
      {page.label}
    </a>
  {/each}
</nav>

<main class="main">
  {#if currentPage === "dashboard"}
    <Dashboard onstatuschange={(r) => runtimeRunning = r} />
  {:else if currentPage === "skills"}
    <SkillEditor />
  {:else if currentPage === "events"}
    <EventViewer />
  {:else if currentPage === "workflows"}
    <WorkflowBoard />
  {:else if currentPage === "soul"}
    <SoulEditor />
  {:else if currentPage === "plugins"}
    <PluginManager />
  {:else if currentPage === "dlq"}
    <DLQ />
  {/if}
</main>
