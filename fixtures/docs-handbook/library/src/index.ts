export interface GreetingOptions {
  name: string;
}

export function greet(options: GreetingOptions): string {
  return `Hello, ${options.name}`;
}
