// Stub: Aman doesn't need REST API calls. Provided for compatibility.
export async function apiFetch(url: string, init?: RequestInit): Promise<Response> {
  throw new Error(`apiFetch not available in Aman: ${url}`);
}
export async function getToken(): Promise<string> {
  return "aman-no-auth";
}
