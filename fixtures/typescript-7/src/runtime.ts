export enum Color {
  Red,
  Green,
  Blue,
}

export const enum Direction {
  Up = 1,
  Down,
}

export namespace Metrics {
  export const base = 7
}

export namespace Metrics {
  export function score(value: number): number {
    return Metrics.base * value
  }
}

export class Point {
  constructor(public x: number, public y: number) {}

  total(): number {
    return this.x + this.y
  }
}

class Resource implements Disposable {
  constructor(readonly value: number) {}

  [Symbol.dispose](): void {}
}

class AsyncResource implements AsyncDisposable {
  constructor(readonly value: number) {}

  async [Symbol.asyncDispose](): Promise<void> {}
}

export function useResource(): number {
  using resource = new Resource(7)
  return resource.value
}

export async function useAsyncResource(): Promise<number> {
  await using resource = new AsyncResource(7)
  return resource.value
}
