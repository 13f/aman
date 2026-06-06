/* Startup Plugin — Alpine.js navigation + interactivity */

function startupNav() {
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

    // Data (initialized from server-rendered page)
    ideaCount: parseInt(document.querySelector('.idea-total')?.textContent || '0'),
    activeCount: 0,

    // ── Layer definitions ─────────────────────────────

    layers: [
      {
        name: 'evaluate', label: 'Evaluation', icon: '🔍',
        modules: [
          { id: 'validate', label: 'Idea Validation', url: '/startup',
            icon: '📊', badge: null },
          { id: 'idea-gen', label: 'Idea Generation', url: '/startup/generate',
            icon: '💡', badge: null },
          { id: 'market', label: 'Market Deep Dive', url: '/startup/market',
            icon: '🌊', badge: null },
        ]
      },
      {
        name: 'strategy', label: 'Strategy', icon: '🎯',
        modules: [
          { id: 'landing-page', label: 'Landing Page', url: '/startup/strategy/landing-page',
            icon: '📄', badge: null },
          { id: 'gtm', label: 'GTM Narrative', url: '/startup/strategy/gtm',
            icon: '📣', badge: null },
          { id: 'pricing-page', label: 'Pricing Page', url: '/startup/strategy/pricing-page',
            icon: '💰', badge: null },
          { id: 'outreach', label: 'Cold Outreach', url: '/startup/strategy/outreach',
            icon: '✉️', badge: null },
        ]
      },
      {
        name: 'execution', label: 'Execution', icon: '⚡',
        modules: [
          { id: 'mvp-scope', label: 'MVP Scope', url: '/startup/execution/mvp-scope',
            icon: '✂️', badge: null },
          { id: 'feedback', label: 'User Feedback', url: '/startup/execution/feedback',
            icon: '🗣️', badge: null },
        ]
      },
      {
        name: 'reflection', label: 'Reflection', icon: '🧘',
        modules: [
          { id: 'journal', label: 'Decision Journal', url: '/startup/reflection/journal',
            icon: '📓', badge: null },
          { id: 'ikigai', label: 'Ikigai Check', url: '/startup/reflection/ikigai',
            icon: '🎌', badge: null },
        ]
      },
      {
        name: 'ai-native', label: 'AI-Native', icon: '🤖',
        modules: [
          { id: 'what-if', label: 'What-If Simulator', url: '/startup/ai-native/what-if',
            icon: '🔄', badge: null },
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

    async navigate(mod) {
      this.currentModule = mod.id;

      // Skip 'soon' badges for now
      if (mod.badge === 'soon') {
        alert(mod.label + ' — coming in a future phase.');
        return;
      }

      try {
        const resp = await fetch(mod.url);
        if (resp.ok) {
          const html = await resp.text();
          document.getElementById('main-content').innerHTML = html;
        }
      } catch (e) {
        console.error('Navigation failed:', e);
      }
    },

    async submitValidate() {
      const body = {
        idea_slug: this.validateForm.slug,
        description: this.validateForm.description,
        keywords: this.validateForm.keywords.split(',').map(k => k.trim()).filter(Boolean),
        niche: this.validateForm.niche,
      };

      try {
        const resp = await fetch('/startup/api/validate', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body),
        });

        const data = await resp.json();
        if (data.ok) {
          this.showValidateModal = false;
          this.validateForm = { slug: '', description: '', keywords: '', niche: '' };
          window.location.reload(); // Refresh to show new idea card
        } else {
          alert('Error: ' + (data.error || 'Unknown error'));
        }
      } catch (e) {
        alert('Request failed: ' + e.message);
      }
    },
  };
}
