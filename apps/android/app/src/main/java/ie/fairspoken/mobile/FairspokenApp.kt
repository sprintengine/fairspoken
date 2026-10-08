package ie.fairspoken.mobile

import android.app.Application
import android.content.Context
import ie.fairspoken.mobile.core.Dictation
import ie.fairspoken.mobile.core.HostClient
import ie.fairspoken.mobile.core.HostStore
import kotlinx.coroutines.MainScope
import kotlinx.coroutines.launch

class FairspokenApp : Application() {
    val scope = MainScope()
    val client = HostClient()
    lateinit var store: HostStore
        private set
    lateinit var dictation: Dictation
        private set

    override fun onCreate() {
        super.onCreate()
        store = HostStore(this)
        dictation = Dictation(this, store, client, scope)
    }

    /** Opens a connection to the active host ahead of the first dictation, and learns its limits. */
    fun warmUp() {
        val host = store.active ?: return
        scope.launch { client.learnLimits(host) }
    }
}

val Context.fairspoken: FairspokenApp
    get() = applicationContext as FairspokenApp
