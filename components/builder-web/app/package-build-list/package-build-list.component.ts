import { Component, OnInit, OnDestroy } from "@angular/core";
import { ActivatedRoute } from "@angular/router";
import { Subscription } from "rxjs/Subscription";
import { fetchBuildLog } from "../actions/index";
import { requireSignIn } from "../util";
import { AppStore } from "../AppStore";

@Component({
  template: `
    <div class="hab-packages">
        <div class="page-title">
            <h2>
              <hab-package-breadcrumbs [ident]="packageParams()"></hab-package-breadcrumbs>
            </h2>
            <h4>
                builds
            </h4>
        </div>
        <div class="page-body">

        </div>
    </div>`
})
export class PackageBuildListComponent implements OnInit, OnDestroy {
  public name: string;
  public origin: string;

  private sub: Subscription;

  constructor(private store: AppStore, private route: ActivatedRoute) {
    requireSignIn(this);

    this.sub = route.params.subscribe(params => {
        this.origin = "core"; // params["origin"];
        this.name = params["name"];
    });
  }

  ngOnInit() {

  }

  ngOnDestroy() {
    this.sub.unsubscribe();
  }

  packageParams() {
      return {
          name: this.name,
          origin: this.origin
      };
  }

  get token() {
      return this.store.getState().gitHub.authToken;
  }
}
