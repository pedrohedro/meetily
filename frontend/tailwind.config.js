/** @type {import('tailwindcss').Config} */
module.exports = {
    darkMode: ['class'],
    content: [
    './src/pages/**/*.{js,ts,jsx,tsx,mdx}',
    './src/components/**/*.{js,ts,jsx,tsx,mdx}',
    './src/app/**/*.{js,ts,jsx,tsx,mdx}',
  ],
  theme: {
  	extend: {
  		fontFamily: {
  			sans: [
  				'var(--font-source-sans-3)'
  			]
  		},
  		colors: {
  			background: 'hsl(var(--background) / <alpha-value>)',
  			foreground: 'hsl(var(--foreground) / <alpha-value>)',
  			// App neutral scale (see globals.css): flips in dark mode, light = Tailwind gray.
  			sidebar: 'hsl(var(--sidebar) / <alpha-value>)',
  			surface: {
  				subtle: 'hsl(var(--surface-subtle) / <alpha-value>)',
  				muted: 'hsl(var(--surface-muted) / <alpha-value>)',
  				strong: 'hsl(var(--surface-strong) / <alpha-value>)',
  				stronger: 'hsl(var(--surface-stronger) / <alpha-value>)'
  			},
  			fg: {
  				strong: 'hsl(var(--fg-strong) / <alpha-value>)',
  				DEFAULT: 'hsl(var(--fg) / <alpha-value>)',
  				soft: 'hsl(var(--fg-soft) / <alpha-value>)',
  				muted: 'hsl(var(--fg-muted) / <alpha-value>)',
  				subtle: 'hsl(var(--fg-subtle) / <alpha-value>)',
  				faint: 'hsl(var(--fg-faint) / <alpha-value>)',
  				ghost: 'hsl(var(--fg-ghost) / <alpha-value>)'
  			},
  			line: {
  				soft: 'hsl(var(--line-soft) / <alpha-value>)',
  				DEFAULT: 'hsl(var(--line) / <alpha-value>)',
  				strong: 'hsl(var(--line-strong) / <alpha-value>)',
  				stronger: 'hsl(var(--line-stronger) / <alpha-value>)'
  			},
  			border: 'hsl(var(--border))',
  			input: 'hsl(var(--input))',
  			ring: 'hsl(var(--ring))',
  			primary: {
  				DEFAULT: 'hsl(var(--primary))',
  				foreground: 'hsl(var(--primary-foreground))'
  			},
  			secondary: {
  				DEFAULT: 'hsl(var(--secondary))',
  				foreground: 'hsl(var(--secondary-foreground))'
  			},
  			tertiary: '#64748b',
  			card: {
  				DEFAULT: 'hsl(var(--card))',
  				foreground: 'hsl(var(--card-foreground))'
  			},
  			popover: {
  				DEFAULT: 'hsl(var(--popover))',
  				foreground: 'hsl(var(--popover-foreground))'
  			},
  			muted: {
  				DEFAULT: 'hsl(var(--muted))',
  				foreground: 'hsl(var(--muted-foreground))'
  			},
  			accent: {
  				DEFAULT: 'hsl(var(--accent))',
  				foreground: 'hsl(var(--accent-foreground))'
  			},
  			destructive: {
  				DEFAULT: 'hsl(var(--destructive))',
  				foreground: 'hsl(var(--destructive-foreground))'
  			},
  			chart: {
  				'1': 'hsl(var(--chart-1))',
  				'2': 'hsl(var(--chart-2))',
  				'3': 'hsl(var(--chart-3))',
  				'4': 'hsl(var(--chart-4))',
  				'5': 'hsl(var(--chart-5))'
  			}
  		},
  		borderRadius: {
  			lg: 'var(--radius)',
  			md: 'calc(var(--radius) - 2px)',
  			sm: 'calc(var(--radius) - 4px)'
  		},
  		keyframes: {
  			'accordion-down': {
  				from: {
  					height: '0'
  				},
  				to: {
  					height: 'var(--radix-accordion-content-height)'
  				}
  			},
  			'accordion-up': {
  				from: {
  					height: 'var(--radix-accordion-content-height)'
  				},
  				to: {
  					height: '0'
  				}
  			}
  		},
  		animation: {
  			'accordion-down': 'accordion-down 0.2s ease-out',
  			'accordion-up': 'accordion-up 0.2s ease-out'
  		}
  	}
  },
  plugins: [
    require("tailwindcss-animate"),
    require("@tailwindcss/typography"),
    require("@tailwindcss/container-queries"),
  ],
}