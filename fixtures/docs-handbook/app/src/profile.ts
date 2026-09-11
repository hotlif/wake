import { loadName } from "./profile-api";
export async function profileTitle(): Promise<string> {
  return `用户：${await loadName()}`;
}
