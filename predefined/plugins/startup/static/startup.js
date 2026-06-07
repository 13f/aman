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
            icon: '📓', badge: 'soon' },
          { id: 'ikigai', label: 'Ikigai Check', url: '/api/v1/startup/reflection/ikigai',
            icon: '🎌', badge: 'soon' },
        ]
      },
      {
        name: 'ai-native', label: 'AI-Native', icon: '🤖',
        modules: [
          { id: 'what-if', label: 'What-If Simulator', url: '/api/v1/startup/ai-native/what-if',
            icon: '🔄', badge: 'soon' },
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

      // Skip 'soon' badges for now
      if (mod.badge === 'soon') {
        alert(mod.label + ' — coming in a future phase.');
        return;
      }

      window.location.href = mod.url;
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
