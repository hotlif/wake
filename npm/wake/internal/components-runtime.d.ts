import type { ComponentType, Key, ReactNode } from 'react'

export { default as Alert } from '@crab-dev/rc-alert'
export { default as Button } from '@crab-dev/rc-button'
export { default as Dialog } from '@crab-dev/rc-dialog'
export { default as Drawer } from '@crab-dev/rc-drawer'
export { default as Empty } from '@crab-dev/rc-empty'
export { default as LineEdit } from '@crab-dev/rc-line-edit'
export { default as NumberEdit } from '@crab-dev/rc-number-edit'
export { default as Segmented } from '@crab-dev/rc-segmented'
export { default as Select } from '@crab-dev/rc-select'
export { default as Switch } from '@crab-dev/rc-switch'
export { default as Tag } from '@crab-dev/rc-tag'
export { default as TextEdit } from '@crab-dev/rc-text-edit'
export { default as Tooltip } from '@crab-dev/rc-tooltip'

export declare enum NodeType {
  FOLDER = 0,
  FILE = 1,
}

export declare enum LoadStateType {
  UNLOADED = 0,
  LOADING = 1,
  LOADING_COMPLETED = 2,
}

export interface TreeNode {
  parent: TreeNode | null
  loadState: LoadStateType
  type: NodeType
  title: ReactNode
  id: Key
  disabled?: boolean
  icon?: ReactNode
  height?: number
  priority?: number
}

export declare const Tree: ComponentType<any>

export {
  Check,
  Code2,
  Copy,
  Menu,
  Monitor,
  Moon,
  RotateCcw,
  SlidersHorizontal,
  Sun,
} from 'lucide-react'
