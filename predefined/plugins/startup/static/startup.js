/* Startup Plugin — Alpine.js navigation + interactivity */

function startupNav() {
  // ── Helpers ──────────────────────────────────────────

  const CACHE_PREFIX = '__startup_cache_';
  const CACHE_TTL_MS = 30 * 60 * 1000;  // 30 minutes

  function _loadCache(modId) {
    try {
      var raw = sessionStorage.getItem(CACHE_PREFIX + modId);
      if (!raw) return null;
      var entry = JSON.parse(raw);
      if (Date.now() - entry.ts > CACHE_TTL_MS) {
        sessionStorage.removeItem(CACHE_PREFIX + modId);
        return null;
      }
      return entry;
    } catch (e) { return null; }
  }

  function _saveCache(modId, data) {
    try {
      sessionStorage.setItem(CACHE_PREFIX + modId, JSON.stringify({
        ts: Date.now(),
        data: data,
      }));
    } catch (e) { /* quota exceeded, ignore */ }
  }

  return {
    // Navigation state
    openLayers: ['evaluate'],
    currentModule: null,
    currentContent: '',

    // Modal state
    showValidateModal: false,
    validateForm: {
      slug: '',
      description: '',
      keywords: '',
      niche: '',
    },

    // Reflection / AI-Native result state
    showResultPanel: false,
    resultPanelTitle: '',
    resultPanelLoading: false,
    resultPanelError: '',
    resultPanelData: null,
    cachedModule: null,       // which module's result is currently cached

    // Data (initialized from server-rendered page)
    ideaCount: parseInt(document.querySelector('.idea-total')?.textContent || '0'),
    activeCount: 0,

    // ── Init: restore cache from sessionStorage ─────────

    restoreCache() {
      // Check if any cached reflection results survive from a prior page load
      for (var _i = 0; _i < this.layers.length; _i++) {
        var layer = this.layers[_i];
        for (var _j = 0; _j < layer.modules.length; _j++) {
          var mod = layer.modules[_j];
          if (mod.action === 'reflection') {
            var entry = _loadCache(mod.id);
            if (entry) {
              this.cachedModule = mod.id;
              this.resultPanelTitle = mod.label;
              this.resultPanelData = entry.data;
              // Don't auto-show — user will click the module to see it
            }
          }
        }
      }
    },

    // ── Layer definitions ─────────────────────────────

    layers: [
      {
        name: 'evaluate', label: 'Evaluation', icon: '🔍',
        modules: [
          { id: 'validate', label: 'Idea Validation', url: '/api/v1/startup',
            icon: '📊', badge: null },
          { id: 'idea-gen', label: 'Idea Generation', url: '/api/v1/startup/generate',
            icon: '💡', badge: 'soon' },
          { id: 'market', label: 'Market Deep Dive', url: '/api/v1/startup/market',
            icon: '🌊', badge: 'soon' },
        ]
      },
      {
        name: 'strategy', label: 'Strategy', icon: '🎯',
        modules: [
          { id: 'landing-page', label: 'Landing Page', url: '/api/v1/startup/strategy/landing-page',
            icon: '📄', badge: 'soon' },
          { id: 'gtm', label: 'GTM Narrative', url: '/api/v1/startup/strategy/gtm',
            icon: '📣', badge: 'soon' },
          { id: 'pricing-page', label: 'Pricing Page', url: '/api/v1/startup/strategy/pricing-page',
            icon: '💰', badge: 'soon' },
          { id: 'outreach', label: 'Cold Outreach', url: '/api/v1/startup/strategy/outreach',
            icon: '✉️', badge: 'soon' },
        ]
      },
      {
        name: 'execution', label: 'Execution', icon: '⚡',
        modules: [
          { id: 'mvp-scope', label: 'MVP Scope', url: '/api/v1/startup/execution/mvp-scope',
            icon: '✂️', badge: 'soon' },
          { id: 'feedback', label: 'User Feedback', url: '/api/v1/startup/execution/feedback',
            icon: '🗣️', badge: 'soon' },
        ]
      },
      {
        name: 'reflection', label: 'Reflection', icon: '🧘',
        modules: [
          { id: 'journal', label: 'Decision Journal', url: '/api/v1/startup/reflection/journal',
            icon: '📓', badge: null, action: 'reflection', apiPath: '/api/v1/startup/api/reflection/journal' },
          { id: 'ikigai', label: 'Ikigai Check', url: '/api/v1/startup/reflection/ikigai',
            icon: '🎌', badge: null, action: 'reflection', apiPath: '/api/v1/startup/api/reflection/ikigai' },
          { id: 'burnout', label: 'Burnout Early Warning', url: '/api/v1/startup/reflection/burnout',
            icon: '🔥', badge: null, action: 'reflection', apiPath: '/api/v1/startup/api/reflection/burnout' },
        ]
      },
      {
        name: 'ai-native', label: 'AI-Native', icon: '🤖',
        modules: [
          { id: 'what-if', label: 'What-If Simulator', url: '/api/v1/startup/ai-native/what-if',
            icon: '🔄', badge: null, action: 'what-if' },
        ]
      },
    ],

    // ── Methods ────────────────────────────────────────

    isOpen(layerName) {
      return this.openLayers.includes(layerName);
    },

    toggle(layerName) {
      if (this.openLayers.includes(layerName)) {
        this.openLayers = this.openLayers.filter(l => l !== layerName);
      } else {
        this.openLayers = [...this.openLayers, layerName];
      }
    },

    navigate(mod) {
      this.currentModule = mod.id;

      // "soon" badges still block
      if (mod.badge === 'soon') {
        alert(mod.label + ' — coming in a future phase.');
        return;
      }

      // Reflection modules: cache-aware (memory + sessionStorage)
      if (mod.action === 'reflection' && mod.apiPath) {
        // Already cached in memory — just show the panel
        if (this.cachedModule === mod.id && this.resultPanelData && !this.resultPanelError) {
          this.showResultPanel = true;
          return;
        }
        // Try sessionStorage (survives page reloads)
        var cached = _loadCache(mod.id);
        if (cached) {
          this.cachedModule = mod.id;
          this.resultPanelTitle = mod.label;
          this.resultPanelData = cached.data;
          this.resultPanelError = '';
          this.resultPanelLoading = false;
          this.showResultPanel = true;
          return;
        }
        this.runReflection(mod);
        return;
      }

      // What-if: show idea picker
      if (mod.action === 'what-if') {
        this.showWhatIfPicker();
        return;
      }

      // Default: navigate to URL
      window.location.href = mod.url;
    },

    // ── Reflection: Ikigai & Journal ────────────────────

    async runReflection(mod, forceRefresh) {
      // If already showing same module and not forcing refresh, just show panel
      if (!forceRefresh && this.cachedModule === mod.id && this.resultPanelData && !this.resultPanelError) {
        this.showResultPanel = true;
        return;
      }

      this.resultPanelTitle = mod.label;
      this.resultPanelLoading = true;
      this.resultPanelError = '';
      if (forceRefresh || this.cachedModule !== mod.id) {
        this.resultPanelData = null;
      }
      this.showResultPanel = true;

      try {
        const resp = await fetch(mod.apiPath, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
        });
        const data = await resp.json();
        if (data.ok && data.result) {
          this.resultPanelData = data.result;
          this.cachedModule = mod.id;
          _saveCache(mod.id, data.result);  // persist across page loads
        } else {
          this.resultPanelError = data.error || 'Unknown error';
          this.cachedModule = null;
        }
      } catch (e) {
        this.resultPanelError = 'Request failed: ' + e.message;
        this.cachedModule = null;
      }
      this.resultPanelLoading = false;
    },

    refreshReflection() {
      // Find the current module definition and re-run
      const mod = this.layers
        .flatMap(l => l.modules)
        .find(m => m.id === this.cachedModule);
      if (mod && mod.action === 'reflection') {
        this.runReflection(mod, true);
      }
    },

    get isReflectionCached() {
      return this.cachedModule && this.resultPanelData && !this.resultPanelLoading;
    },

    // ── What-If Picker ──────────────────────────────────

    showWhatIfPicker() {
      this.resultPanelTitle = 'What-If Simulator';
      this.resultPanelLoading = false;
      this.resultPanelError = '';
      this.resultPanelData = { _whatIfPicker: true };
      this.cachedModule = 'what-if';
      this.showResultPanel = true;
    },

    get whatIfIdeas() {
      // Read from server-injected data instead of DOM scraping.
      // This works even when the idea grid is hidden.
      var raw = window.__startupIdeas;
      if (raw && raw.length) return raw;
      // Fallback: try DOM (only works when idea grid is visible)
      var cards = document.querySelectorAll('.idea-card');
      var ideas = [];
      cards.forEach(function(card) {
        var h3 = card.querySelector('h3');
        var onclick = card.getAttribute('onclick') || '';
        var m = onclick.match(/ideas\/([^'"]+)/);
        if (h3 && m) ideas.push({ slug: m[1], label: h3.textContent });
      });
      return ideas;
    },

    async runWhatIf(slug, question) {
      if (!slug) {
        this.resultPanelError = 'Please select an idea.';
        return;
      }
      if (!question.trim()) {
        this.resultPanelError = 'Please describe your what-if scenario.';
        return;
      }

      this.resultPanelLoading = true;
      this.resultPanelError = '';
      this.resultPanelData = null;

      try {
        const resp = await fetch('/api/v1/startup/api/ideas/' + encodeURIComponent(slug) + '/what-if', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ question: question.trim() }),
        });
        const data = await resp.json();
        if (data.ok && data.result) {
          this.resultPanelData = data.result;
        } else {
          this.resultPanelError = data.error || 'Unknown error';
        }
      } catch (e) {
        this.resultPanelError = 'Request failed: ' + e.message;
      }
      this.resultPanelLoading = false;
    },

    closeResultPanel() {
      this.showResultPanel = false;
    },

    // ── Helpers for template rendering ──────────────────

    isIkigaiResult() {
      return this.resultPanelData && this.resultPanelData.quadrant_scores;
    },

    isJournalResult() {
      return this.resultPanelData && (this.resultPanelData.decision_quality_score !== undefined || this.resultPanelData.detected_biases !== undefined);
    },

    isBurnoutResult() {
      return this.resultPanelData && this.resultPanelData.risk_score !== undefined;
    },

    isWhatIfResult() {
      return this.resultPanelData && this.resultPanelData.cascading_effects;
    },

    async submitValidate() {
      const body = {
        idea_slug: this.validateForm.slug,
        description: this.validateForm.description,
        keywords: this.validateForm.keywords.split(',').map(k => k.trim()).filter(Boolean),
        niche: this.validateForm.niche,
      };

      try {
        const resp = await fetch('/api/v1/startup/api/validate', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });

        const data = await resp.json();
        if (data.ok) {
          this.showValidateModal = false;
          this.validateForm = { slug: '', description: '', keywords: '', niche: '' };
          window.location.reload();
        } else {
          alert('Error: ' + (data.error || 'Unknown error'));
        }
      } catch (e) {
        alert('Request failed: ' + e.message);
      }
    },
  };
}
