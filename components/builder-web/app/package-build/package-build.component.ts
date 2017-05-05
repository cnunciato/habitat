import { Component, OnInit, OnDestroy } from "@angular/core";
import { ActivatedRoute } from "@angular/router";
import { Subscription } from "rxjs/Subscription";
import { fetchBuildLog } from "../actions/index";
import { requireSignIn } from "../util";
import { AppStore } from "../AppStore";

@Component({
  template: `
    <div class="hab-package-build">
        <div class="page-title">
            <h2>
              <hab-package-breadcrumbs [ident]="packageParams()"></hab-package-breadcrumbs>
            </h2>
            <h4>
                {{ id }}
            </h4>
        </div>
        <div class="page-body">
          <pre class="output" *ngIf="log" [innerHTML]="log"></pre>
        </div>
    </div>`
})
export class PackageBuildComponent implements OnInit, OnDestroy {
  public name: string;
  public origin: string;
  public id: string;

  private _log: string;
  private sub: Subscription;

  constructor(private store: AppStore, private route: ActivatedRoute) {
    requireSignIn(this);

    this.sub = route.params.subscribe(params => {
        this.origin = "core"; // params["origin"];
        this.name = params["name"];
        this.id = params["id"];
    });
  }

  ngOnInit() {
    this.store.dispatch(fetchBuildLog(this.id, this.token));
  }

  ngOnDestroy() {
    this.sub.unsubscribe();
  }

  packageParams() {
      return {
          name: this.name,
          origin: this.origin,
          version: "builds" // eew! sorry!
      };
  }

  get log() {
    let o = this.store.getState().projects.current.buildLog;
    let c;

    if (o.content) {
      c = o.content.join("\n");
    }

    if (this._log && c !== this._log) {
      document.body.scrollTop = document.body.scrollHeight - 1200;
    }

    this._log = c;
    return c;
  }

  get token() {
      return this.store.getState().gitHub.authToken;
  }
}
