export interface Named {
  readonly name: string
}

function logged<This, Args extends unknown[], Result>(
  target: (this: This, ...args: Args) => Result,
  context: ClassMethodDecoratorContext<This, (this: This, ...args: Args) => Result>,
) {
  return function (this: This, ...args: Args): Result {
    if (context.kind !== 'method') throw new TypeError('Expected a method')
    return target.call(this, ...args)
  }
}

export abstract class Entity implements Named {
  abstract readonly name: string

  constructor(public readonly id: string, private rank = 1) {}

  protected score(): number {
    return this.rank
  }
}

export class User extends Entity {
  accessor nickname = 'wake'

  constructor(id: string, public override readonly name: string) {
    super(id)
  }

  @logged
  label(suffix = ''): string {
    return `${this.name}:${this.score()}${suffix}`
  }
}

declare class AmbientService {
  readonly ready: boolean
}

export type ServiceState = AmbientService['ready']
