import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    console.info('[Phantom] EntryAbility onCreate');
  }

  onDestroy(): void {
    console.info('[Phantom] EntryAbility onDestroy');
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err) => {
      if (err) {
        console.error('[Phantom] loadContent error: ' + JSON.stringify(err));
        return;
      }
      console.info('[Phantom] loadContent success');
    });
  }
}
