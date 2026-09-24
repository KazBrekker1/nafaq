export default defineAppConfig({
  ui: {
    colors: {
      primary: 'violet',
      neutral: 'neutral'
    },
    button: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard) hover:translate-x-[calc(var(--ui-shadow-offset-x)/2)] hover:translate-y-[calc(var(--ui-shadow-offset-y)/2)] hover:shadow-(--ui-shadow-hard-half) active:translate-x-(--ui-shadow-offset-x) active:translate-y-(--ui-shadow-offset-y) active:shadow-none transition-[box-shadow,translate,background-color]'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none hover:translate-x-0 hover:translate-y-0 hover:shadow-none active:translate-x-0 active:translate-y-0'
        },
        {
          variant: 'link',
          class: 'shadow-none hover:translate-x-0 hover:translate-y-0 hover:shadow-none active:translate-x-0 active:translate-y-0'
        }
      ]
    },
    badge: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard-sm)'
      }
    },
    input: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard)'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none'
        },
        {
          variant: 'none',
          class: 'shadow-none'
        }
      ]
    },
    select: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard)',
        content: 'shadow-(--ui-shadow-hard-lg)'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none'
        },
        {
          variant: 'none',
          class: 'shadow-none'
        }
      ]
    },
    textarea: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard)'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none'
        },
        {
          variant: 'none',
          class: 'shadow-none'
        }
      ]
    },
    selectMenu: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard)',
        content: 'shadow-(--ui-shadow-hard-lg)'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none'
        },
        {
          variant: 'none',
          class: 'shadow-none'
        }
      ]
    },
    inputMenu: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      },
      slots: {
        base: 'shadow-(--ui-shadow-hard)',
        content: 'shadow-(--ui-shadow-hard-lg)'
      },
      compoundVariants: [
        {
          variant: 'ghost',
          class: 'shadow-none'
        },
        {
          variant: 'none',
          class: 'shadow-none'
        }
      ]
    },
    inputNumber: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      }
    },
    inputTags: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      }
    },
    inputDate: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      }
    },
    inputTime: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      }
    },
    pinInput: {
      defaultVariants: {
        variant: 'subtle',
        size: 'xs'
      }
    },
    inputRating: {
      defaultVariants: {
        size: 'xs'
      }
    },
    tabs: {
      defaultVariants: {
        size: 'xs'
      }
    },
    checkbox: {
      defaultVariants: {
        size: 'xs'
      }
    },
    checkboxGroup: {
      defaultVariants: {
        size: 'xs'
      }
    },
    radioGroup: {
      defaultVariants: {
        size: 'xs'
      }
    },
    switch: {
      defaultVariants: {
        size: 'xs'
      }
    },
    slider: {
      defaultVariants: {
        size: 'xs'
      }
    },
    stepper: {
      defaultVariants: {
        size: 'xs'
      }
    },
    calendar: {
      defaultVariants: {
        size: 'xs'
      }
    },
    colorPicker: {
      defaultVariants: {
        size: 'xs'
      }
    },
    fileUpload: {
      defaultVariants: {
        size: 'xs'
      }
    },
    formField: {
      defaultVariants: {
        size: 'xs'
      }
    },
    dropdownMenu: {
      defaultVariants: {
        size: 'xs'
      },
      slots: {
        content: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    contextMenu: {
      defaultVariants: {
        size: 'xs'
      },
      slots: {
        content: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    commandPalette: {
      defaultVariants: {
        size: 'xs'
      }
    },
    listbox: {
      defaultVariants: {
        size: 'xs'
      }
    },
    card: {
      slots: {
        root: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    empty: {
      slots: {
        root: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    alert: {
      slots: {
        root: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    popover: {
      slots: {
        content: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    tooltip: {
      slots: {
        content: 'shadow-(--ui-shadow-hard-sm)'
      }
    },
    toast: {
      slots: {
        root: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    drawer: {
      slots: {
        content: 'shadow-(--ui-shadow-hard-lg)'
      }
    },
    modal: {
      compoundVariants: [
        {
          fullscreen: false,
          class: {
            content: 'shadow-(--ui-shadow-hard-lg)'
          }
        }
      ]
    },
    slideover: {
      slots: {
        content: 'sm:shadow-(--ui-shadow-hard-lg)'
      }
    }
  }
})
