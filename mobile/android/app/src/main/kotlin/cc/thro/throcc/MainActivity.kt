package cc.thro.throcc

import android.content.pm.ApplicationInfo
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.initializer
import androidx.lifecycle.viewmodel.viewModelFactory
import cc.thro.throcc.core.Command
import cc.thro.throcc.core.Core
import cc.thro.throcc.core.CoreException

/** Why the core couldn't open what this install keeps. */
@Composable
private fun StartFailure(reason: String) {
    MaterialTheme(if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        Surface(Modifier.fillMaxSize()) {
            Text(reason, Modifier.safeDrawingPadding().padding(24.dp))
        }
    }
}

class MainActivity : ComponentActivity() {
    private var model: AppModel? = null

    /** Opens the core, or in a debuggable build with the `mock` extra the sample one at the page `screen` names. */
    private val models = viewModelFactory {
        initializer {
            val debuggable = applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
            val mock = debuggable && intent.getBooleanExtra("mock", false)
            val sample = intent.getStringExtra("screen").takeIf { mock }
            AppModel(if (mock) Core.mock(onboarded = sample != "onboarding") else Core(), sample)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        try {
            val model = ViewModelProvider(this, models)[AppModel::class]
            this.model = model
            setContent { App(model) }
        } catch (failure: CoreException) {
            setContent { StartFailure(failure.message.orEmpty()) }
        }
    }

    override fun onStart() {
        super.onStart()
        model?.core?.command(Command.Reconnect)
    }
}
