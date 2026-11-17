import { createApp } from 'vue'
import App from './App.vue'
import './style.css'
import { activeLanguage } from './i18n.js'

document.documentElement.lang = activeLanguage()
createApp(App).mount('#app')
