/**
 * Re-eksport rejestru providerów z shared (bliźniak theme/mapping.ts i theme/models.ts).
 * Trzyma importy klienta przy jednej ścieżce '../theme/providers'.
 */
export { AGENT_PROVIDERS, resolveProvider } from '../shared/index';
export type { ProviderInfo } from '../shared/index';
